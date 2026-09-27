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
use std::time::Duration;

use ninfer_monitor::chart;
use ninfer_monitor::columns::{self, Column, ColumnWidths, TableColumns};
use ninfer_monitor::config::{self, Config};
use ninfer_monitor::gpu::{self, GpuHistory, GpuPoller, GpuSnapshot, TempUnit};
use ninfer_monitor::kpi;
use ninfer_monitor::nvidia_popup;
use ninfer_monitor::pipeline::{Pipeline, StoreUpdate, format_last_event, shorten_file_name};
use ninfer_monitor::requests;
use ninfer_monitor::server_info;
use ninfer_monitor::split::SplitPanels;
use ninfer_monitor::startup;
use ninfer_monitor::store::Store;
use ninfer_monitor::tail::TailStatus;
use ninfer_monitor::update_check::{self, LatestRelease};
use ninfer_monitor::window_state::{self, RestorePlan, WindowState};
use slint::ComponentHandle;
use slint::{CloseRequestResponse, LogicalPosition, LogicalSize, WindowPosition, WindowSize};
use slint::{Model, ModelRc};

const COLOR_LIVE: slint::Color = slint::Color::from_rgb_u8(0x4c, 0xaf, 0x50);
const COLOR_DISCONNECTED: slint::Color = slint::Color::from_rgb_u8(0xf4, 0x43, 0x36);
const COLOR_NONE: slint::Color = slint::Color::from_rgb_u8(0x9e, 0x9e, 0x9e);

/// The time resolution at which the charts' right edge (the current time,
/// M5) advances between re-renders: a frame whose right edge has not moved a
/// full second would be unchanged, so it is not re-rendered.
const CHART_TIME_QUANTUM_MS: u64 = 1_000;

/// The key of a rendered main-charts frame: (window, panel sizes, data epoch,
/// right-edge second); an unchanged frame has an equal key.
type ChartFrameKey = (u64, [(u32, u32); 4], u64, u64);

/// The key of a rendered GPU-rows frame: (window, chart size, display unit,
/// data epoch, right-edge second); an unchanged frame has an equal key.
type GpuFrameKey = (u64, (u32, u32), TempUnit, u64, u64);

fn status_color(status: TailStatus) -> slint::Color {
    match status {
        TailStatus::Live => COLOR_LIVE,
        TailStatus::Disconnected => COLOR_DISCONNECTED,
    }
}

/// The tail/parse pipeline and the file it tails, shared between the event
/// loop and the control handlers (FR-7.1).
struct App {
    pipeline: Option<Pipeline>,
    path: Option<PathBuf>,
}

/// Automatic update-check state (M8), shared between the event loop and the
/// background checker thread: the current settings, the persisted
/// last-successful-check time, the failed-attempt throttle, and the release
/// the last successful check found.
struct UpdateState {
    enabled: bool,
    period_days: u64,
    last_check_unix_ms: Option<u64>,
    last_attempt_unix_ms: Option<u64>,
    available: Option<LatestRelease>,
}

/// How often the checker thread re-evaluates the due condition (M8).
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// The minimum time between two check attempts (M8): a failed check must not
/// be re-attempted on every evaluation, but the persisted schedule is kept,
/// so the next scheduled check still retries.
const UPDATE_CHECK_RETRY_AFTER_MS: u64 = 3_600_000;

/// The background update checker (M8): a thread that evaluates the due
/// condition and runs the (network) check without blocking the UI.
struct UpdateChecker {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl UpdateChecker {
    /// Start the checker; the first due check runs immediately (the
    /// startup check, M8 §5.1).
    fn start(
        update: Arc<Mutex<UpdateState>>,
        config_path: Option<PathBuf>,
        weak: slint::Weak<MainWindow>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let handle = Some(std::thread::spawn(move || {
            loop {
                attempt_update_check(&update, config_path.as_deref(), &weak);
                if !sleep_or_stop(&stop_flag, UPDATE_CHECK_INTERVAL) {
                    break;
                }
            }
        }));
        Self { stop, handle }
    }

    /// Signal the checker thread to stop and join it.
    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Sleep for `total`, re-checking the stop flag every 100 ms so a stop
/// request is honored promptly. Returns `false` when stopping.
fn sleep_or_stop(stop: &AtomicBool, total: Duration) -> bool {
    const STEP: Duration = Duration::from_millis(100);
    let mut remaining = total;
    while remaining > Duration::ZERO {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let step = if remaining > STEP { STEP } else { remaining };
        std::thread::sleep(step);
        remaining -= step;
    }
    !stop.load(Ordering::SeqCst)
}

/// Chart rendering state shared by the tick and window-change handlers.
/// `window_ms` is the source of truth for the chart window; the slint
/// `chart-window-ms` property is a read-only mirror of it. The charts are
/// re-rendered on every UI tick in which the frame could change (M5);
/// `last_chart_key` holds the key of the last rendered frame so an unchanged
/// frame is skipped.
struct ChartState {
    window_ms: u64,
    store: Store,
    table_fingerprint: u64,
    /// Request ids whose inline detail is expanded in the table (FR-5.4).
    expanded_ids: HashSet<u64>,
    /// The `server_start_count` last seen; a higher value means the server
    /// restarted and the (now stale) expanded ids must be cleared.
    server_start_count: usize,
    server_info: Option<server_info::ServerInfoSnapshot>,
    /// The sizes the charts were last rendered at, so each plot matches its
    /// panel aspect (no distortion).
    render_sizes: [(u32, u32); 4],
    /// Bumped whenever `store` is replaced, so fresh data always re-renders.
    data_epoch: u64,
    /// The key of the last rendered frame; an equal key means the frame
    /// would be unchanged.
    last_chart_key: Option<ChartFrameKey>,
}

fn new_chart_state(window_ms: u64, max_requests: u64) -> ChartState {
    ChartState {
        window_ms,
        store: Store::with_max_requests(max_requests as usize),
        table_fingerprint: 0,
        expanded_ids: HashSet::new(),
        server_start_count: 0,
        server_info: None,
        render_sizes: [(chart::CHART_WIDTH, chart::CHART_HEIGHT); 4],
        data_epoch: 0,
        last_chart_key: None,
    }
}

/// GPU state shared by the tick and settings handlers (M4). `poller` is the
/// background NVML poller; `state` holds the latest snapshot, the bounded
/// history, the display unit, and the render bookkeeping.
struct Gpu {
    poller: Option<GpuPoller>,
    state: GpuState,
}

/// GPU render state (M4). `snapshot` is the latest poll; `history` is the
/// bounded time series; `unit` is the display unit. The GPU rows are
/// re-rendered on every UI tick in which the frame could change (M5);
/// `last_gpu_key` holds the key of the last rendered frame so an unchanged
/// frame is skipped.
struct GpuState {
    history: GpuHistory,
    snapshot: GpuSnapshot,
    unit: TempUnit,
    /// Bumped whenever a new poll is drained, so fresh readings re-render.
    data_epoch: u64,
    /// The key of the last rendered frame; an equal key means the frame
    /// would be unchanged.
    last_gpu_key: Option<GpuFrameKey>,
}

fn new_gpu_state(unit: TempUnit) -> GpuState {
    GpuState {
        history: GpuHistory::new(),
        snapshot: GpuSnapshot::unavailable(),
        unit,
        data_epoch: 0,
        last_gpu_key: None,
    }
}

/// The GPU chart render size (M4): the throughput chart-rect size, read from
/// the widget out properties.
fn gpu_chart_size(window: &MainWindow) -> (u32, u32) {
    chart_panel_size(
        window.get_widget_throughput_w(),
        window.get_widget_throughput_h(),
    )
}

/// Map a `gpu::GpuRowData` to the slint `GpuDevice` model (M4).
fn to_slint_gpu(r: &gpu::GpuRowData) -> GpuDevice {
    GpuDevice {
        usage_title: r.usage_title.clone().into(),
        usage_left_label: r.usage_left_label.clone().into(),
        usage_left_value: r.usage_left_value.clone().into(),
        usage_left_unit: r.usage_left_unit.clone().into(),
        usage_right_label: r.usage_right_label.clone().into(),
        usage_right_value: r.usage_right_value.clone().into(),
        usage_image: r.usage_image.clone(),
        temp_title: r.temp_title.clone().into(),
        temp_left_label: r.temp_left_label.clone().into(),
        temp_left_value: r.temp_left_value.clone().into(),
        temp_left_unit: r.temp_left_unit.clone().into(),
        temp_right_label: r.temp_right_label.clone().into(),
        temp_right_value: r.temp_right_value.clone().into(),
        temp_right_unit: r.temp_right_unit.clone().into(),
        temp_image: r.temp_image.clone(),
    }
}

/// Drain the newest GPU snapshot into the history and refresh the GPU rows
/// (M4). Called from the ≤ 4 Hz tick.
fn tick_gpu(window: &MainWindow, gpu: &Arc<Mutex<Gpu>>, window_ms: u64) {
    let mut g = gpu.lock().unwrap();
    if let Some(snap) = g.poller.as_ref().and_then(|p| p.take_latest()) {
        g.state.snapshot = snap;
        g.state.data_epoch += 1;
        let sample = gpu::sample_from_snapshot(&g.state.snapshot, gpu::now_ms());
        g.state.history.push(sample);
    }
    apply_gpu(window, &mut g.state, window_ms);
}

/// Re-render the GPU rows whenever the frame could change (M5: new data, a
/// size/window/unit change, or the right edge advancing a full second), so
/// the dashed stale tails track the current time without re-rendering an
/// unchanged frame. An unavailable snapshot yields an empty model (the GPU
/// section is hidden).
fn apply_gpu(window: &MainWindow, state: &mut GpuState, window_ms: u64) {
    if !state.snapshot.available {
        if window.get_gpu_devices().row_count() != 0 {
            window.set_gpu_devices(ModelRc::from(Rc::new(slint::VecModel::<GpuDevice>::from(
                Vec::new(),
            ))));
        }
        return;
    }
    let size = gpu_chart_size(window);
    let now = gpu::now_ms();
    let key = (
        window_ms,
        size,
        state.unit,
        state.data_epoch,
        now / CHART_TIME_QUANTUM_MS,
    );
    if state.last_gpu_key == Some(key) {
        return;
    }
    state.last_gpu_key = Some(key);
    let rows = gpu::build_rows(
        &state.snapshot,
        &state.history,
        state.unit,
        window_ms,
        size,
        now,
    );
    let slint_rows: Vec<GpuDevice> = rows.iter().map(to_slint_gpu).collect();
    window.set_gpu_devices(ModelRc::from(Rc::new(slint::VecModel::from(slint_rows))));
}

/// Re-render the GPU rows on a chart-window change (M4), mirroring the main
/// charts' immediate re-render so the two chart groups do not momentarily
/// show different windows. The new window changes the render key, so the
/// rows are rebuilt at once; a no-op when the GPU section is not shown.
fn refresh_gpu_window(window: &MainWindow, gpu: &Arc<Mutex<Gpu>>, window_ms: u64) {
    let mut g = gpu.lock().unwrap();
    apply_gpu(window, &mut g.state, window_ms);
}

/// The actual on-screen size of a chart panel, read from the widget out
/// properties. Falls back to the default render size when the layout has not
/// run yet (e.g. in tests), so charts are never rendered at size 0.
fn chart_panel_size(w: f32, h: f32) -> (u32, u32) {
    if w <= 0.0 || h <= 0.0 {
        (chart::CHART_WIDTH, chart::CHART_HEIGHT)
    } else {
        (w as u32, h as u32)
    }
}

/// The actual on-screen size of each chart panel, read from the widget out
/// properties.
fn chart_panel_sizes(window: &MainWindow) -> [(u32, u32); 4] {
    [
        chart_panel_size(
            window.get_widget_throughput_w(),
            window.get_widget_throughput_h(),
        ),
        chart_panel_size(window.get_widget_latency_w(), window.get_widget_latency_h()),
        chart_panel_size(window.get_widget_cache_w(), window.get_widget_cache_h()),
        chart_panel_size(
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
            detail_materialization: r.detail.materialization.clone().into(),
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

/// The NVIDIA profile CLI actions (M6); the matching flag runs the action and
/// exits the process before the GUI starts (PRD v0.2 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NvidiaProfileAction {
    Add,
    Delete,
}

struct CliArgs {
    config_path: Option<PathBuf>,
    nvidia_profile: Option<NvidiaProfileAction>,
}

fn parse_cli_args(args: &[String]) -> CliArgs {
    let mut config_path = None;
    let mut nvidia_profile = None;
    let mut i = 0;
    while i < args.len() {
        // A flag's value must not itself be a flag, so `--config --other x`
        // leaves `--other` for its own handling. Both `--flag value` and
        // `--flag=value` forms are accepted; a missing value warns on stderr
        // and is ignored.
        let arg = args[i].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag == "--config" => (flag, Some(value)),
            _ => (arg, None),
        };
        match flag {
            "--config" => match inline {
                Some(value) => config_path = Some(PathBuf::from(value)),
                None if i + 1 < args.len() && !args[i + 1].starts_with('-') => {
                    i += 1;
                    config_path = Some(PathBuf::from(args[i].as_str()));
                }
                None => eprintln!("warning: --config expects a path value"),
            },
            "--add-nvidia-app-profile" => nvidia_profile = Some(NvidiaProfileAction::Add),
            "--delete-nvidia-app-profile" => nvidia_profile = Some(NvidiaProfileAction::Delete),
            other if other.starts_with('-') => {}
            other => eprintln!("warning: ignoring unexpected argument {other:?}"),
        }
        i += 1;
    }
    CliArgs {
        config_path,
        nvidia_profile,
    }
}

/// Runs the NVIDIA profile CLI action and exits (M6): on success it prints
/// `added`/`removed` to stdout and exits with code 0; on failure it prints the
/// error to stderr and exits with code 1. The process exits before any GUI
/// work (PRD v0.2 §4.2).
fn run_nvidia_profile_cli(action: NvidiaProfileAction) -> ! {
    #[cfg(windows)]
    attach_parent_console();
    let (success_message, result) = match action {
        NvidiaProfileAction::Add => ("added", nvidia_popup::add_profile()),
        NvidiaProfileAction::Delete => ("removed", nvidia_popup::remove_profile()),
    };
    match result {
        Ok(()) => {
            println!("{success_message}");
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

/// Attaches the process to the parent console so the CLI output is visible in
/// a terminal (the app is a Windows GUI subsystem and does not allocate a
/// console of its own). A no-op when there is no parent console, e.g. under
/// the installer's `ExecWait`.
#[cfg(windows)]
fn attach_parent_console() {
    use windows::Win32::System::Console::AttachConsole;
    // SAFETY: a single in-range constant (`ATTACH_PARENT_PROCESS`).
    let _ = unsafe { AttachConsole(u32::MAX) };
}

/// Effective startup settings (FR-1.1/FR-7.2): the log path, log file poll
/// interval, GPU poll interval, chart window, and max request rows all come
/// from the config.
struct Resolved {
    path: Option<PathBuf>,
    log_poll_interval: Duration,
    gpu_poll_interval_ms: u64,
    chart_window_ms: u64,
    max_requests: u64,
}

fn resolve(cfg: &Config) -> Resolved {
    Resolved {
        path: cfg.last_path.clone().map(PathBuf::from),
        log_poll_interval: cfg.log_poll_interval(),
        gpu_poll_interval_ms: cfg.gpu_poll_interval().as_millis() as u64,
        chart_window_ms: cfg.chart_window_ms(),
        max_requests: cfg.max_requests(),
    }
}

/// Load the config from `cli_config_path` (or the default location,
/// FR-7.2), clamp out-of-range stored values, and persist the normalized
/// config when the file is missing (created on first run), malformed
/// (repaired), or its stored values were out of range. The file is read
/// once.
fn load_startup_config(cli_config_path: Option<&Path>) -> (Option<PathBuf>, Config) {
    let _write = config::write_lock();
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
    let clamped_log_poll = cfg
        .log_poll_interval_ms
        .clamp(config::MIN_LOG_POLL_MS, config::MAX_LOG_POLL_MS);
    let clamped_gpu_poll = cfg
        .gpu_poll_interval_ms
        .clamp(config::MIN_GPU_POLL_MS, config::MAX_GPU_POLL_MS);
    let clamped_window = cfg
        .chart_window_ms
        .clamp(config::MIN_WINDOW_MS, config::MAX_WINDOW_MS);
    let clamped_max_requests = cfg
        .max_requests
        .clamp(config::MIN_MAX_REQUESTS, config::MAX_MAX_REQUESTS);
    let clamped_update_period = update_check::clamp_period_days(cfg.update_check_period_days);
    let clamped_split = cfg.split_panels.clamped();
    let changed = clamped_log_poll != cfg.log_poll_interval_ms
        || clamped_gpu_poll != cfg.gpu_poll_interval_ms
        || clamped_window != cfg.chart_window_ms
        || clamped_max_requests != cfg.max_requests
        || clamped_update_period != cfg.update_check_period_days
        || clamped_split != cfg.split_panels;
    cfg.log_poll_interval_ms = clamped_log_poll;
    cfg.gpu_poll_interval_ms = clamped_gpu_poll;
    cfg.chart_window_ms = clamped_window;
    cfg.max_requests = clamped_max_requests;
    cfg.update_check_period_days = clamped_update_period;
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

/// Apply the startup UI state (FR-7.1): show the file name when a log is
/// opened.
fn apply_startup_state(window: &MainWindow, path: Option<&Path>) {
    if let Some(path) = path {
        set_file_name(window, path);
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

/// Log file poll interval and max request rows for a pipeline started by
/// "Open file…" (FR-7.1/FR-7.3): the (possibly externally edited) config
/// values are re-read.
fn settings_for_new_file(config_path: Option<&Path>) -> (Duration, u64) {
    let cfg = config_path.map(config::load).unwrap_or_default();
    (cfg.log_poll_interval(), cfg.max_requests())
}

/// Persist the newly opened log path as `last_path` (FR-7.2), keeping the
/// other settings.
fn persist_last_path(config_path: Option<&Path>, path: &Path) {
    let Some(cp) = config_path else {
        return;
    };
    let _write = config::write_lock();
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
/// leaves the default size/position in place. Returns whether the window is
/// restored maximized (the pre-`run` width is then the last normal or default
/// width, which the column-restore decision relies on, M7).
fn apply_window_state(window: &MainWindow, saved: Option<WindowState>) -> bool {
    let monitors = window_state::current_monitors();
    let RestorePlan::Restore {
        x,
        y,
        width,
        height,
        maximized,
    } = window_state::resolve_restore(saved.as_ref(), &monitors)
    else {
        return false;
    };
    let win = window.window();
    win.set_position(WindowPosition::Logical(LogicalPosition::new(x, y)));
    win.set_size(WindowSize::Logical(LogicalSize::new(width, height)));
    win.set_maximized(maximized);
    maximized
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
    let _write = config::write_lock();
    let mut cfg = config::load(cp);
    cfg.window_state = Some(state);
    let _ = config::save(cp, &cfg);
}

/// Reset the dashboard to the empty state: KPIs to "—", charts to "no
/// data", table cleared, server info reset. Used when a new file is opened.
fn reset_view(window: &MainWindow, state: &mut ChartState) {
    let empty = Store::with_max_requests(state.store.max_requests());
    let k = kpi::snapshot(&empty);
    ninfer_monitor::apply_kpis!(window, &k);
    let images =
        chart::render_all_sized(&empty, state.window_ms, &state.render_sizes, gpu::now_ms());
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
    state.data_epoch += 1;
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

/// Apply the live view (KPIs, server info, table) from `store`. The charts
/// are re-rendered by `render_charts` when the frame could change (M5).
fn apply_view(window: &MainWindow, state: &mut ChartState, store: &Store) {
    let k = kpi::snapshot(store);
    ninfer_monitor::apply_kpis!(window, &k);
    window.set_schema_warning(schema_warning(store).into());
    let info = server_info::snapshot(store);
    if server_info::needs_update(&state.server_info, &info) {
        ninfer_monitor::apply_server_info!(window, &info);
        state.server_info = Some(info);
    }
    apply_table(window, state, store, false);
}

/// Re-render the main charts from the stored snapshot whenever the frame
/// could change (M5: new data, a panel/window change, or the right edge
/// advancing a full second), so the dashed stale tails track the current
/// time without re-rendering an unchanged frame. A no-op when the store is
/// empty (the "no data" images never change).
fn render_charts(window: &MainWindow, state: &mut ChartState) {
    if state.store.max_timestamp_ms().is_none() {
        return;
    }
    state.render_sizes = chart_panel_sizes(window);
    let now = gpu::now_ms();
    let key = (
        state.window_ms,
        state.render_sizes,
        state.data_epoch,
        now / CHART_TIME_QUANTUM_MS,
    );
    if state.last_chart_key == Some(key) {
        return;
    }
    state.last_chart_key = Some(key);
    let images = chart::render_all_sized(&state.store, state.window_ms, &state.render_sizes, now);
    ninfer_monitor::apply_chart_images!(window, &images);
}

/// Apply the newest pending snapshot to the UI.
///
/// Called from the ≤ 4 Hz tick and on a chart-window change so the
/// re-render uses the freshest store. The connection status/color are
/// refreshed on every update since they track the tail status, which is
/// independent of the in-memory data.
///
/// Returns `true` when a snapshot was applied.
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
    let mut state = charts.lock().unwrap();
    apply_status_bar(window, &path, &update);
    window.set_connection_status(update.status.to_string().into());
    window.set_status_color(status_color(update.status));
    apply_view(window, &mut state, &update.store);
    state.data_epoch += 1;
    state.store = update.store;
    true
}

/// Chart-window handler (FR-7.1): update the window, re-render, and persist
/// the setting. The newest pending snapshot is applied first so the
/// re-render uses the freshest store, not one up to a tick old.
fn handle_window_changed(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
    gpu: &Arc<Mutex<Gpu>>,
    config_path: Option<&Path>,
    ms: i32,
) {
    let window_ms = (ms as u64).clamp(config::MIN_WINDOW_MS, config::MAX_WINDOW_MS);
    window.set_chart_window_ms(window_ms as i32);
    // Set the new window before draining a snapshot so that any chart render
    // uses the new window, not the old one.
    {
        let mut state = charts.lock().unwrap();
        state.window_ms = window_ms;
    }
    // Drain the newest snapshot so the re-render uses the freshest store,
    // not one up to a tick old, then re-render the charts at once with the
    // new window (M5), so they do not wait for the next tick.
    apply_latest(window, app, charts);
    {
        let mut state = charts.lock().unwrap();
        render_charts(window, &mut state);
    }
    refresh_gpu_window(window, gpu, window_ms);
    if let Some(cp) = config_path {
        let _write = config::write_lock();
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
        let _write = config::write_lock();
        let mut cfg = config::load(cp);
        if cfg.split_panels != s {
            cfg.split_panels = s;
            let _ = config::save(cp, &cfg);
        }
    }
}

/// The main window's width in logical px (M7): the window size divided by
/// the scale factor (the table column widths are stored in logical px, like
/// the persisted window geometry).
fn logical_window_width(window: &MainWindow) -> f32 {
    let win = window.window();
    win.size().width as f32 / win.scale_factor()
}

/// Set one table column width in the UI (M7).
fn set_column_width(window: &MainWindow, col: Column, width: u32) {
    match col {
        Column::Id => window.set_col_id(width as f32),
        Column::Time => window.set_col_time(width as f32),
        Column::Model => window.set_col_model(width as f32),
        Column::Prompt => window.set_col_prompt(width as f32),
        Column::Compl => window.set_col_compl(width as f32),
        Column::Think => window.set_col_think(width as f32),
        Column::Ttft => window.set_col_ttft(width as f32),
        Column::Total => window.set_col_total(width as f32),
        Column::Reason => window.set_col_reason(width as f32),
        Column::Status => window.set_col_status(width as f32),
    }
}

/// Push all ten column widths to the UI (M7).
fn apply_column_widths(window: &MainWindow, widths: &ColumnWidths) {
    for (i, col) in Column::ALL.iter().enumerate() {
        set_column_width(window, *col, widths.widths[i]);
    }
}

/// Column-width tick (M7): rebalance the flex `Model` column to the current
/// window width (a window resize must keep the columns filling the table)
/// and apply the widths when they changed.
fn tick_columns(window: &MainWindow, table_columns: &Arc<Mutex<ColumnWidths>>) {
    let mut c = table_columns.lock().unwrap();
    let before = c.widths;
    columns::rebalance(logical_window_width(window), &mut c);
    if c.widths != before {
        apply_column_widths(window, &c);
    }
}

/// Column-divider drag handler (M7): move the divider between its two
/// columns (the total width stays constant, both stay at or above their
/// minimums) and push the updated widths to the UI so the table re-lays out
/// live. Persistence happens on release (`handle_column_released`), not on
/// every move, so a drag does not hit the disk on each mouse event.
fn handle_column_changed(
    window: &MainWindow,
    table_columns: &Arc<Mutex<ColumnWidths>>,
    slider: i32,
    new_left: f32,
) {
    let Some((left, right)) = columns::slider_columns(slider) else {
        return;
    };
    let updated = {
        let mut c = table_columns.lock().unwrap();
        *c = c.move_divider(left, right, new_left);
        c.widths
    };
    set_column_width(window, left, updated[left.index()]);
    set_column_width(window, right, updated[right.index()]);
}

/// Column-divider release handler (M7): persist the current column widths
/// together with the (logical px) window width they were saved at, keeping
/// the other settings.
fn handle_column_released(
    window: &MainWindow,
    config_path: Option<&Path>,
    table_columns: &Arc<Mutex<ColumnWidths>>,
) {
    let Some(cp) = config_path else {
        return;
    };
    let saved =
        TableColumns::from_widths(logical_window_width(window), &table_columns.lock().unwrap());
    let _write = config::write_lock();
    let mut cfg = config::load(cp);
    if cfg.table_columns != Some(saved) {
        cfg.table_columns = Some(saved);
        let _ = config::save(cp, &cfg);
    }
}

/// Settings-open handler (FR-7.1/M3): show the settings dialog pre-filled
/// with the current log file poll interval, GPU poll interval, max request
/// rows, the update-check settings (M8), and the tailed log file (empty when
/// no file is tailed, e.g. while the startup screen is shown), clearing the
/// last NVIDIA popup status.
fn handle_settings_open(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    update: &Arc<Mutex<UpdateState>>,
    config_path: Option<&Path>,
) {
    let cfg = config_path.map(config::load).unwrap_or_default();
    window.set_settings_log_poll_ms(cfg.log_poll_interval_ms as i32);
    window.set_settings_gpu_poll_ms(cfg.gpu_poll_interval_ms as i32);
    window.set_settings_max_requests(cfg.max_requests() as i32);
    window.set_settings_temp_unit(cfg.temp_unit().as_str().into());
    let (update_enabled, update_days) = {
        let u = update.lock().unwrap();
        (u.enabled, u.period_days)
    };
    window.set_settings_update_check_enabled(update_enabled);
    window.set_settings_update_check_days(update_days as i32);
    let file = app
        .lock()
        .unwrap()
        .path
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    window.set_settings_file(file.into());
    window.set_nvidia_popup_status(String::new().into());
    window.set_settings_visible(true);
}

/// Settings-save handler (FR-7.1/M3/M8): persist the log file poll interval,
/// the GPU poll interval, the max request rows, and the update-check
/// settings, close the dialog, and — when any of the tailed file, the log
/// file poll interval, or the max request rows changed — restart the tail
/// (reopen, backfill, reset the view, persist the new path) so the new
/// settings take effect immediately instead of only for the next opened
/// file. A missing or unreadable file is still switched to: the tail shows
/// Disconnected and retries until the file becomes readable. An empty file
/// field keeps the current file (no switch). Disabling the update check
/// clears the update-available state immediately (M8 §5.1).
#[allow(clippy::too_many_arguments)]
fn handle_settings_save(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
    gpu: &Arc<Mutex<Gpu>>,
    config_path: Option<&Path>,
    log_poll_ms: i32,
    gpu_poll_ms: i32,
    max_requests: i32,
    file: &str,
    temp_unit: &str,
    update_check_enabled: bool,
    update_check_days: i32,
    update: &Arc<Mutex<UpdateState>>,
) {
    let log_poll_ms = (log_poll_ms as u64).clamp(config::MIN_LOG_POLL_MS, config::MAX_LOG_POLL_MS);
    let gpu_poll_ms = (gpu_poll_ms as u64).clamp(config::MIN_GPU_POLL_MS, config::MAX_GPU_POLL_MS);
    let max_requests =
        (max_requests as u64).clamp(config::MIN_MAX_REQUESTS, config::MAX_MAX_REQUESTS);
    let unit = TempUnit::parse(temp_unit);
    let update_check_days = (update_check_days as u64)
        .clamp(update_check::MIN_PERIOD_DAYS, update_check::MAX_PERIOD_DAYS);
    if let Some(cp) = config_path {
        let _write = config::write_lock();
        let mut cfg = config::load(cp);
        cfg.log_poll_interval_ms = log_poll_ms;
        cfg.gpu_poll_interval_ms = gpu_poll_ms;
        cfg.max_requests = max_requests;
        cfg.temp_unit = unit.as_str().to_owned();
        cfg.update_check_enabled = update_check_enabled;
        cfg.update_check_period_days = update_check_days;
        let _ = config::save(cp, &cfg);
    }
    // Apply the temperature unit and the new GPU poll interval immediately
    // (M4); neither requires a tail restart.
    {
        let mut g = gpu.lock().unwrap();
        g.state.unit = unit;
        if let Some(p) = g.poller.as_ref() {
            p.set_poll_interval(gpu_poll_ms);
        }
    }
    // Apply the update-check settings immediately (M8); disabling the check
    // also clears the update-available state.
    {
        let mut u = update.lock().unwrap();
        u.enabled = update_check_enabled;
        u.period_days = update_check_days;
        if !update_check_enabled {
            u.available = None;
        }
    }
    if !update_check_enabled {
        window.set_update_available(false);
    }
    window.set_about_visible(false);
    window.set_settings_visible(false);
    let file = file.trim();
    if file.is_empty() {
        return;
    }
    let new_path = PathBuf::from(file);
    let (current, current_poll, current_max) = {
        let app = app.lock().unwrap();
        let (poll, max) = match app.pipeline.as_ref() {
            Some(p) => (Some(p.poll_interval()), Some(p.max_requests())),
            None => (None, None),
        };
        (app.path.clone(), poll, max)
    };
    let new_poll = Duration::from_millis(log_poll_ms);
    let file_changed = current.as_deref() != Some(new_path.as_path());
    let poll_changed = current_poll != Some(new_poll);
    let max_changed = current_max != Some(max_requests as usize);
    if !file_changed && !poll_changed && !max_changed {
        return;
    }
    handle_open_file(window, app, charts, config_path, new_path);
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

/// "Disable NVIDIA popup" outcome handler (M6): shows the result of the
/// profile operation — the success message or the error string — directly
/// below the settings link.
fn apply_nvidia_popup_result(window: &MainWindow, result: Result<(), String>) {
    let status = match result {
        Ok(()) => nvidia_popup::SUCCESS_MESSAGE.to_owned(),
        Err(error) => error,
    };
    window.set_nvidia_popup_status(status.into());
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
    path: PathBuf,
) {
    let (log_poll_interval, max_requests) = settings_for_new_file(config_path);
    let old_pipeline = {
        let mut app = app.lock().unwrap();
        let old = app.pipeline.take();
        app.pipeline = Some(Pipeline::start(
            &path,
            log_poll_interval,
            max_requests as usize,
        ));
        app.path = Some(path.clone());
        old
    };
    if let Some(old) = old_pipeline {
        std::thread::spawn(move || old.stop());
    }
    set_file_name(window, &path);
    window.set_connection_status("—".into());
    window.set_status_color(COLOR_NONE);
    let mut state = charts.lock().unwrap();
    reset_view(window, &mut state);
    persist_last_path(config_path, &path);
}

/// Start handler (PRD v0.2 §3.1/M1): re-check the selected file's validity at
/// click time, then start the pipeline and switch the window to the dashboard.
/// A file that became invalid since the enabled state was last computed is
/// ignored (the app does not wait for a file that appears later).
fn handle_startup_start(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
    config_path: Option<&Path>,
) {
    let path = PathBuf::from(window.get_startup_file().as_str());
    if !startup::file_is_readable(&path) {
        return;
    }
    handle_open_file(window, app, charts, config_path, path);
    window.set_startup_visible(false);
}

/// Close handler (PRD v0.2 §3.1/M1): persist the window geometry (FR-7.4) and
/// exit the application. No pipeline is running while the startup screen is
/// shown, so there is nothing to stop.
fn handle_startup_close(
    window: &MainWindow,
    config_path: Option<&Path>,
    tracked_state: &Mutex<Option<WindowState>>,
) {
    let state = window_state_to_save(window, *tracked_state.lock().unwrap());
    persist_window_state(config_path, state);
    std::process::exit(0);
}

/// File-edited handler (PRD v0.2 §3.1/M1): update the Start button's enabled
/// state to reflect whether the edited path is a valid (existing, readable)
/// file. The state is only recomputed on edits (no retry or auto-enable).
fn handle_startup_file_edited(window: &MainWindow, text: &str) {
    window.set_startup_start_enabled(startup::file_is_readable(Path::new(text)));
}

/// Run the scheduled update check with an explicit time and an injectable
/// checker (M8; the network `update_check::check` in production). A check
/// runs only when the check is enabled, the persisted period has elapsed
/// since the last successful check, and the failed-attempt throttle has
/// elapsed. A successful check records `last_check_unix_ms` (persisted,
/// M8 §5.1) and the found release — the release only when the check is
/// still enabled (the settings may disable it mid-flight, and a disabled
/// check must not re-show the update button); a failed check keeps the
/// previous state. Returns whether a check ran.
fn run_update_check_with<F>(
    update: &Arc<Mutex<UpdateState>>,
    config_path: Option<&Path>,
    now_unix_ms: u64,
    check: F,
) -> bool
where
    F: FnOnce() -> Result<Option<LatestRelease>, update_check::CheckError>,
{
    let due = {
        let s = update.lock().unwrap();
        s.enabled
            && update_check::is_check_due(s.last_check_unix_ms, s.period_days, now_unix_ms)
            && s.last_attempt_unix_ms
                .is_none_or(|t| now_unix_ms.saturating_sub(t) >= UPDATE_CHECK_RETRY_AFTER_MS)
    };
    if !due {
        return false;
    }
    update.lock().unwrap().last_attempt_unix_ms = Some(now_unix_ms);
    let outcome = check();
    let success = {
        let mut s = update.lock().unwrap();
        match outcome {
            Ok(release) => {
                s.last_check_unix_ms = Some(now_unix_ms);
                if s.enabled {
                    s.available = release;
                }
                true
            }
            Err(_) => false,
        }
    };
    if success {
        persist_last_update_check(config_path, now_unix_ms);
    }
    true
}

/// The production check entry point (M8): evaluate the schedule and run the
/// network check, mirroring the outcome into the UI on the event loop. A
/// failed check is silent (no dialog, no status entry, M8 §5.1).
fn attempt_update_check(
    update: &Arc<Mutex<UpdateState>>,
    config_path: Option<&Path>,
    weak: &slint::Weak<MainWindow>,
) {
    let now = update_check::now_unix_ms();
    if !run_update_check_with(update, config_path, now, || {
        update_check::check(env!("CARGO_PKG_VERSION"))
    }) {
        return;
    }
    let available = update.lock().unwrap().available.clone();
    let weak = weak.clone();
    let invoked = slint::invoke_from_event_loop(move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        apply_update_available(&window, available.as_ref());
    });
    if invoked.is_err() {
        // The event loop is gone; the state is kept for the next start.
    }
}

/// Mirror the current update-available state into the UI (M8): the toolbar
/// button is visible only when an update is available, and the dialog fields
/// carry the new release's version and release page.
fn apply_update_available(window: &MainWindow, available: Option<&LatestRelease>) {
    match available {
        Some(release) => {
            window.set_update_version(release.version.clone().into());
            window.set_update_release_url(release.url.clone().into());
            window.set_update_available(true);
        }
        None => window.set_update_available(false),
    }
}

/// Persist the last successful update-check time (M8 §5.1), keeping the
/// other settings; skipped without a config path.
fn persist_last_update_check(config_path: Option<&Path>, unix_ms: u64) {
    let Some(config_path) = config_path else {
        return;
    };
    let _write = config::write_lock();
    let mut cfg = config::load(config_path);
    cfg.last_update_check_unix_ms = Some(unix_ms);
    let _ = config::save(config_path, &cfg);
}

/// Update-button handler (M8): show the "New version available" dialog.
fn handle_update_clicked(window: &MainWindow) {
    window.set_update_visible(true);
}

/// Update-dialog-close handler (M8): hide the dialog. The update button
/// stays visible so the dialog can be reopened later. A download in flight
/// is canceled by the caller (the close callback sets the cancel flag), so
/// this also serves as the download cancel.
fn handle_update_close(window: &MainWindow) {
    window.set_update_visible(false);
}

/// "Release notes" handler (M8): open the latest release page in the system
/// browser.
fn handle_release_notes(window: &MainWindow) {
    let url = window.get_update_release_url().to_string();
    let _ = update_check::open_in_browser(&url);
}

/// Update-dialog OK handler (M8 §5.3): download the release installer for
/// the current platform in a background thread (the event loop stays
/// responsive, NFR-2). The download result is handled on the event loop
/// (`apply_install_result`), where the cancel flag is read atomically with
/// the dialog-close handler that sets it. On success the window state is
/// persisted and the event loop is quit; `main` then stops the worker
/// threads and launches the installer. On failure the error is shown in the
/// dialog so the user can retry or cancel. A cancel (Cancel/Escape/backdrop)
/// while the download runs removes the downloaded file and resets the dialog
/// state, so a reopened dialog is fresh.
fn handle_update_install(
    window: &MainWindow,
    update: &Arc<Mutex<UpdateState>>,
    in_flight: &Arc<AtomicBool>,
    cancel: &Arc<AtomicBool>,
    pending: &Arc<Mutex<Option<PathBuf>>>,
    tracked_state: &Arc<Mutex<Option<WindowState>>>,
    config_path: Option<PathBuf>,
) {
    // Ignore re-clicks while a download is already in flight.
    if in_flight
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    cancel.store(false, Ordering::SeqCst);
    let Some(release) = update.lock().unwrap().available.clone() else {
        in_flight.store(false, Ordering::SeqCst);
        return;
    };
    window.set_update_downloading(true);
    let weak = window.as_weak();
    let in_flight = in_flight.clone();
    let cancel = cancel.clone();
    let pending = pending.clone();
    let tracked_state = tracked_state.clone();
    std::thread::spawn(move || {
        let result = update_check::download_installer(&release);
        let guard = in_flight.clone();
        let invoked = slint::invoke_from_event_loop(move || {
            guard.store(false, Ordering::SeqCst);
            // The cancel flag is checked on the event-loop thread, so the
            // check is atomic with the dialog-close handler that sets it.
            let canceled = cancel.load(Ordering::SeqCst);
            let Some(w) = weak.upgrade() else {
                return;
            };
            if apply_install_result(
                &w,
                result,
                canceled,
                &pending,
                &tracked_state,
                config_path.as_deref(),
            ) {
                let _ = slint::quit_event_loop();
            }
        });
        if invoked.is_err() {
            in_flight.store(false, Ordering::SeqCst);
        }
    });
}

/// Apply the installer-download result on the event loop (M8 §5.3),
/// returning `true` when the event loop should quit (a confirmed update).
/// A canceled download removes the downloaded (possibly partial) file and
/// resets the dialog state so a reopened dialog is fresh. On success the
/// installer path is stored in `pending` for the post-event-loop launch in
/// `main` and the window state is persisted. On failure the error is shown
/// so the user can retry or cancel.
fn apply_install_result(
    window: &MainWindow,
    result: Result<PathBuf, update_check::InstallError>,
    canceled: bool,
    pending: &Arc<Mutex<Option<PathBuf>>>,
    tracked_state: &Arc<Mutex<Option<WindowState>>>,
    config_path: Option<&Path>,
) -> bool {
    if canceled {
        if let Ok(path) = &result {
            let _ = std::fs::remove_file(path);
        }
        window.set_update_downloading(false);
        window.set_update_download_error("".into());
        return false;
    }
    match result {
        Ok(path) => {
            *pending.lock().unwrap() = Some(path);
            // The event loop quits without a close request, so persist the
            // window state the way the close handler does.
            let state = window_state_to_save(window, *tracked_state.lock().unwrap());
            persist_window_state(config_path, state);
            true
        }
        Err(e) => {
            // Keep the dialog open so the user can retry or cancel.
            window.set_update_downloading(false);
            window.set_update_download_error(e.to_string().into());
            false
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = parse_cli_args(&args);
    if let Some(action) = cli.nvidia_profile {
        run_nvidia_profile_cli(action);
    }

    let (config_path, cfg) = load_startup_config(cli.config_path.as_deref());
    let resolved = resolve(&cfg);
    let startup = startup::startup_state(resolved.path.as_deref());

    let window = MainWindow::new()?;
    window.set_about_version(env!("CARGO_PKG_VERSION").into());
    window.set_chart_window_ms(resolved.chart_window_ms as i32);
    // The "Disable NVIDIA popup" link is Windows-only (M6).
    window.set_nvidia_popup_visible(cfg!(windows));
    // The install OK button is Windows-only (M8): the packager builds an
    // installer for Windows only.
    window.set_update_ok_visible(cfg!(windows));
    let restored_maximized = apply_window_state(&window, cfg.window_state);
    // Restore the request-table column widths (M7): the saved widths apply
    // only when the window width is the same as when they were saved,
    // otherwise the defaults are used — except when the window opens
    // maximized, where the pre-`run` width is the normal (or default) width,
    // not the maximized width the saved widths may have been recorded at.
    // The flex `Model` column is then rebalanced to the actual window width.
    let window_width = logical_window_width(&window);
    let mut table_widths =
        columns::resolve_restore(cfg.table_columns.as_ref(), window_width, restored_maximized);
    columns::rebalance(window_width, &mut table_widths);
    apply_column_widths(&window, &table_widths);
    let table_columns = Arc::new(Mutex::new(table_widths));
    // Restore the split-panel fractions (FR-7.5); the UI derives the section
    // heights from them.
    window.set_charts_frac(cfg.split_panels.charts);
    window.set_table_frac(cfg.split_panels.table);
    let split = Arc::new(Mutex::new(cfg.split_panels));
    let charts = Arc::new(Mutex::new(new_chart_state(
        resolved.chart_window_ms,
        resolved.max_requests,
    )));
    let gpu = Arc::new(Mutex::new(Gpu {
        poller: Some(GpuPoller::start(resolved.gpu_poll_interval_ms)),
        state: new_gpu_state(cfg.temp_unit()),
    }));
    let app = Arc::new(Mutex::new(App {
        pipeline: if startup.visible {
            None
        } else {
            resolved.path.as_ref().map(|p| {
                Pipeline::start(
                    p,
                    resolved.log_poll_interval,
                    resolved.max_requests as usize,
                )
            })
        },
        path: if startup.visible {
            None
        } else {
            resolved.path.clone()
        },
    }));
    // Automatic update-check state (M8); the background checker runs the
    // startup check immediately and then re-evaluates the schedule.
    let update = Arc::new(Mutex::new(UpdateState {
        enabled: cfg.update_check_enabled,
        period_days: cfg.update_check_period_days(),
        last_check_unix_ms: cfg.last_update_check_unix_ms,
        last_attempt_unix_ms: None,
        available: None,
    }));
    if startup.visible {
        window.set_startup_visible(true);
        window.set_startup_version(env!("CARGO_PKG_VERSION").into());
        window.set_startup_message(startup.message.into());
        window.set_startup_file(startup.file.into());
        window.set_startup_start_enabled(false);
    } else {
        apply_startup_state(&window, resolved.path.as_deref());
    }
    let tracked_state = Arc::new(Mutex::new(None::<WindowState>));
    // Update-install flow (M8 §5.3): `install-in-flight` guards against
    // re-clicks, `install-cancel` is set by the dialog close while a
    // download runs, and `pending-install` hands the downloaded installer to
    // the post-event-loop launch below.
    let install_in_flight = Arc::new(AtomicBool::new(false));
    let install_cancel = Arc::new(AtomicBool::new(false));
    let pending_install: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));

    window.on_window_changed({
        let weak = window.as_weak();
        let charts = charts.clone();
        let app = app.clone();
        let gpu = gpu.clone();
        let config_path = config_path.clone();
        move |ms: i32| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_window_changed(&ui_window, &app, &charts, &gpu, config_path.as_deref(), ms);
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
    window.on_column_changed({
        let weak = window.as_weak();
        let table_columns = table_columns.clone();
        move |slider: i32, new_left: f32| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_column_changed(&ui_window, &table_columns, slider, new_left);
        }
    });
    window.on_column_released({
        let weak = window.as_weak();
        let config_path = config_path.clone();
        let table_columns = table_columns.clone();
        move |_slider: i32| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_column_released(&ui_window, config_path.as_deref(), &table_columns);
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
    window.on_settings({
        let weak = window.as_weak();
        let app = app.clone();
        let update = update.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_settings_open(&ui_window, &app, &update, config_path.as_deref());
        }
    });
    window.on_settings_save({
        let weak = window.as_weak();
        let app = app.clone();
        let charts = charts.clone();
        let gpu = gpu.clone();
        let update = update.clone();
        let config_path = config_path.clone();
        move |log_poll_ms: i32,
              gpu_poll_ms: i32,
              max_requests: i32,
              file: slint::SharedString,
              temp_unit: slint::SharedString,
              update_check_enabled: bool,
              update_check_days: i32| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_settings_save(
                &ui_window,
                &app,
                &charts,
                &gpu,
                config_path.as_deref(),
                log_poll_ms,
                gpu_poll_ms,
                max_requests,
                file.as_str(),
                temp_unit.as_str(),
                update_check_enabled,
                update_check_days,
                &update,
            );
        }
    });
    window.on_settings_open_file({
        let weak = window.as_weak();
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
            let dialog_open = dialog_open.clone();
            std::thread::spawn(move || {
                let path = pick_log_file();
                let guard = dialog_open.clone();
                let invoked = slint::invoke_from_event_loop(move || {
                    guard.store(false, Ordering::SeqCst);
                    let Some(w) = weak.upgrade() else {
                        return;
                    };
                    let Some(path) = path else {
                        return;
                    };
                    w.set_settings_file(path.display().to_string().into());
                });
                if invoked.is_err() {
                    dialog_open.store(false, Ordering::SeqCst);
                }
            });
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
    window.on_update_clicked({
        let weak = window.as_weak();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_update_clicked(&ui_window);
        }
    });
    window.on_update_close({
        let weak = window.as_weak();
        let install_cancel = install_cancel.clone();
        move || {
            // Also cancels a download in flight (M8 §5.3).
            install_cancel.store(true, Ordering::SeqCst);
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_update_close(&ui_window);
        }
    });
    window.on_update_install({
        let weak = window.as_weak();
        let update = update.clone();
        let install_in_flight = install_in_flight.clone();
        let install_cancel = install_cancel.clone();
        let pending_install = pending_install.clone();
        let tracked_state = tracked_state.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_update_install(
                &ui_window,
                &update,
                &install_in_flight,
                &install_cancel,
                &pending_install,
                &tracked_state,
                config_path.clone(),
            );
        }
    });
    window.on_release_notes({
        let weak = window.as_weak();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_release_notes(&ui_window);
        }
    });
    window.on_nvidia_popup({
        let weak = window.as_weak();
        let running = Arc::new(AtomicBool::new(false));
        move || {
            // Ignore re-clicks while a profile operation is in flight.
            if running
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            let weak = weak.clone();
            let running = running.clone();
            std::thread::spawn(move || {
                let result = nvidia_popup::add_profile_interactive();
                let guard = running.clone();
                let invoked = slint::invoke_from_event_loop(move || {
                    guard.store(false, Ordering::SeqCst);
                    let Some(w) = weak.upgrade() else {
                        return;
                    };
                    apply_nvidia_popup_result(&w, result);
                });
                if invoked.is_err() {
                    running.store(false, Ordering::SeqCst);
                }
            });
        }
    });
    window.on_startup_start({
        let weak = window.as_weak();
        let charts = charts.clone();
        let app = app.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_startup_start(&ui_window, &app, &charts, config_path.as_deref());
        }
    });
    window.on_startup_close({
        let weak = window.as_weak();
        let config_path = config_path.clone();
        let tracked_state = tracked_state.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_startup_close(&ui_window, config_path.as_deref(), &tracked_state);
        }
    });
    window.on_startup_open_file({
        let weak = window.as_weak();
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
            let dialog_open = dialog_open.clone();
            std::thread::spawn(move || {
                let path = pick_log_file();
                let guard = dialog_open.clone();
                let invoked = slint::invoke_from_event_loop(move || {
                    guard.store(false, Ordering::SeqCst);
                    let Some(w) = weak.upgrade() else {
                        return;
                    };
                    let Some(path) = path else {
                        return;
                    };
                    w.set_startup_file(path.display().to_string().into());
                    w.set_startup_start_enabled(startup::file_is_readable(&path));
                });
                if invoked.is_err() {
                    dialog_open.store(false, Ordering::SeqCst);
                }
            });
        }
    });
    window.on_startup_file_edited({
        let weak = window.as_weak();
        move |text: slint::SharedString| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_startup_file_edited(&ui_window, text.as_str());
        }
    });
    window.on_tick({
        let weak = window.as_weak();
        let charts = charts.clone();
        let app = app.clone();
        let gpu = gpu.clone();
        let table_columns = table_columns.clone();
        let tracked_state = tracked_state.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            apply_latest(&ui_window, &app, &charts);
            // Re-render the charts when the frame could change (M5): the
            // dashed stale tails must track the current time.
            let window_ms = {
                let mut state = charts.lock().unwrap();
                render_charts(&ui_window, &mut state);
                state.window_ms
            };
            tick_gpu(&ui_window, &gpu, window_ms);
            // Rebalance the table columns to the current window width (M7).
            tick_columns(&ui_window, &table_columns);
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

    let update_checker =
        UpdateChecker::start(update.clone(), config_path.clone(), window.as_weak());

    window.run()?;

    update_checker.stop();
    if let Some(pipeline) = app.lock().unwrap().pipeline.take() {
        pipeline.stop();
    }
    if let Some(poller) = gpu.lock().unwrap().poller.take() {
        poller.stop();
    }
    // A confirmed update (M8 §5.3): the event loop and all worker threads
    // have stopped, so the installer can replace the running executable.
    if let Some(installer) = pending_install.lock().unwrap().take() {
        match std::process::Command::new(&installer).spawn() {
            Ok(_) => std::process::exit(0),
            Err(e) => report_installer_launch_failure(&e),
        }
    }
    Ok(())
}

/// Report a failed installer launch (M8 §5.3). On Windows the app is a GUI
/// subsystem with no console, so the diagnostic goes to the debug output
/// (visible in DebugView) instead of a terminal.
#[cfg(windows)]
fn report_installer_launch_failure(error: &std::io::Error) {
    use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
    use windows::core::PCWSTR;
    let message = format!("NInferMonitor: failed to start the installer: {error}\0");
    let wide: Vec<u16> = message.encode_utf16().collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 string; `message` (and thus
    // `wide`) is alive for the duration of the call.
    unsafe { OutputDebugStringW(PCWSTR(wide.as_ptr())) }
}

#[cfg(not(windows))]
fn report_installer_launch_failure(error: &std::io::Error) {
    eprintln!("failed to start the installer: {error}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use ninfer_monitor::split;
    use slint::Model;
    use std::time::Instant;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    /// A GPU state with no poller, for tests that exercise the settings
    /// handlers (M4).
    fn test_gpu() -> Arc<Mutex<Gpu>> {
        Arc::new(Mutex::new(Gpu {
            poller: None,
            state: new_gpu_state(TempUnit::Fahrenheit),
        }))
    }

    fn make_config(
        last_path: Option<&str>,
        log_poll_ms: u64,
        window_ms: u64,
        max_requests: u64,
    ) -> Config {
        Config {
            last_path: last_path.map(str::to_owned),
            log_poll_interval_ms: log_poll_ms,
            gpu_poll_interval_ms: config::DEFAULT_GPU_POLL_MS,
            chart_window_ms: window_ms,
            max_requests,
            temp_unit: "F".to_owned(),
            split_panels: SplitPanels::default(),
            table_columns: None,
            window_state: None,
            update_check_enabled: true,
            update_check_period_days: update_check::DEFAULT_PERIOD_DAYS,
            last_update_check_unix_ms: None,
        }
    }

    /// An enabled update state with no history, for tests that exercise the
    /// settings handlers (M8).
    fn test_update() -> Arc<Mutex<UpdateState>> {
        Arc::new(Mutex::new(UpdateState {
            enabled: true,
            period_days: update_check::DEFAULT_PERIOD_DAYS,
            last_check_unix_ms: None,
            last_attempt_unix_ms: None,
            available: None,
        }))
    }

    #[test]
    fn positional_argument_is_ignored() {
        let cli = parse_cli_args(&args(&["logs/server.requests.jsonl"]));
        assert_eq!(cli.config_path, None);
    }

    #[test]
    fn no_args_yields_defaults() {
        let cli = parse_cli_args(&args(&[]));
        assert_eq!(cli.config_path, None);
    }

    #[test]
    fn unknown_flags_are_skipped() {
        let cli = parse_cli_args(&args(&["--verbose", "logs/x.jsonl"]));
        assert_eq!(cli.config_path, None);
    }

    #[test]
    fn config_flag_value_is_consumed_as_config_path() {
        let cli = parse_cli_args(&args(&["--config", "cfg.json", "logs/x.jsonl"]));
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg.json")));
    }

    #[test]
    fn config_flag_without_value_is_ignored() {
        let cli = parse_cli_args(&args(&["--config"]));
        assert_eq!(cli.config_path, None);
    }

    #[test]
    fn config_flag_does_not_consume_another_flag() {
        let cli = parse_cli_args(&args(&["--config", "--verbose", "100"]));
        assert_eq!(cli.config_path, None);
    }

    #[test]
    fn config_equals_form_is_parsed() {
        let cli = parse_cli_args(&args(&["--config=cfg.json", "logs/x.jsonl"]));
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg.json")));
    }

    #[test]
    fn nvidia_add_flag_is_parsed() {
        let cli = parse_cli_args(&args(&["--add-nvidia-app-profile"]));
        assert_eq!(cli.nvidia_profile, Some(NvidiaProfileAction::Add));
        assert_eq!(cli.config_path, None);
    }

    #[test]
    fn nvidia_delete_flag_is_parsed() {
        let cli = parse_cli_args(&args(&["--delete-nvidia-app-profile"]));
        assert_eq!(cli.nvidia_profile, Some(NvidiaProfileAction::Delete));
    }

    #[test]
    fn nvidia_profile_flags_coexist_with_config() {
        let cli = parse_cli_args(&args(&["--config", "cfg.json", "--add-nvidia-app-profile"]));
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg.json")));
        assert_eq!(cli.nvidia_profile, Some(NvidiaProfileAction::Add));
    }

    #[test]
    fn last_nvidia_profile_flag_wins() {
        let cli = parse_cli_args(&args(&[
            "--add-nvidia-app-profile",
            "--delete-nvidia-app-profile",
        ]));
        assert_eq!(cli.nvidia_profile, Some(NvidiaProfileAction::Delete));
    }

    #[test]
    fn nvidia_popup_success_shows_the_prd_message() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        apply_nvidia_popup_result(&window, Ok(()));
        assert_eq!(
            window.get_nvidia_popup_status(),
            "NVIDIA game popup disabled."
        );
    }

    #[test]
    fn nvidia_popup_error_shows_the_error() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        apply_nvidia_popup_result(&window, Err("no NVIDIA driver".to_owned()));
        assert_eq!(window.get_nvidia_popup_status(), "no NVIDIA driver");
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
            r#"{"log_poll_interval_ms":1,"gpu_poll_interval_ms":999999999,"chart_window_ms":999999999,"max_requests":999999}"#,
        )
        .unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(cfg.log_poll_interval_ms, config::MIN_LOG_POLL_MS);
        assert_eq!(cfg.gpu_poll_interval_ms, config::MAX_GPU_POLL_MS);
        assert_eq!(cfg.chart_window_ms, config::MAX_WINDOW_MS);
        assert_eq!(cfg.max_requests, config::MAX_MAX_REQUESTS);
        let saved = config::load(&cp);
        assert_eq!(saved.log_poll_interval_ms, config::MIN_LOG_POLL_MS);
        assert_eq!(saved.gpu_poll_interval_ms, config::MAX_GPU_POLL_MS);
        assert_eq!(saved.chart_window_ms, config::MAX_WINDOW_MS);
        assert_eq!(saved.max_requests, config::MAX_MAX_REQUESTS);
    }

    #[test]
    fn startup_config_clamps_single_out_of_range_field() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":1,"chart_window_ms":300000}"#,
        )
        .unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(cfg.log_poll_interval_ms, config::MIN_LOG_POLL_MS);
        assert_eq!(cfg.gpu_poll_interval_ms, config::DEFAULT_GPU_POLL_MS);
        assert_eq!(cfg.chart_window_ms, 300_000);
        assert_eq!(cfg.max_requests, config::DEFAULT_MAX_REQUESTS);
        let saved = config::load(&cp);
        assert_eq!(saved.log_poll_interval_ms, config::MIN_LOG_POLL_MS);
        assert_eq!(saved.gpu_poll_interval_ms, config::DEFAULT_GPU_POLL_MS);
        assert_eq!(saved.chart_window_ms, 300_000);
        assert_eq!(saved.max_requests, config::DEFAULT_MAX_REQUESTS);
    }

    #[test]
    fn startup_config_in_range_file_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":200,"gpu_poll_interval_ms":3000,"chart_window_ms":60000,"sentinel":1}"#,
        )
        .unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(cfg.log_poll_interval_ms, 200);
        assert_eq!(cfg.gpu_poll_interval_ms, 3_000);
        assert_eq!(cfg.chart_window_ms, 60_000);
        let raw = std::fs::read_to_string(&cp).unwrap();
        assert!(
            raw.contains("sentinel"),
            "an in-range config must not be rewritten: {raw}"
        );
    }

    #[test]
    fn open_file_settings_come_from_config() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"log_poll_interval_ms":200,"max_requests":250}"#).unwrap();
        let (poll, max_requests) = settings_for_new_file(Some(&cp));
        assert_eq!(poll, Duration::from_millis(200));
        assert_eq!(max_requests, 250);
    }

    #[test]
    fn open_file_settings_defaults_without_config() {
        let (poll, max_requests) = settings_for_new_file(None);
        assert_eq!(poll, Config::default().log_poll_interval());
        assert_eq!(max_requests, config::DEFAULT_MAX_REQUESTS);
    }

    #[test]
    fn persist_last_path_keeps_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":200,"chart_window_ms":60000}"#,
        )
        .unwrap();
        persist_last_path(Some(&cp), Path::new("logs/x.jsonl"));
        let cfg = config::load(&cp);
        assert_eq!(cfg.last_path, Some("logs/x.jsonl".to_owned()));
        assert_eq!(cfg.log_poll_interval_ms, 200);
        assert_eq!(cfg.chart_window_ms, 60_000);
    }

    #[test]
    fn persist_last_path_without_config_is_a_noop() {
        persist_last_path(None, Path::new("logs/x.jsonl"));
    }

    #[test]
    fn startup_state_sets_file_name() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        apply_startup_state(&window, None);
        assert_eq!(window.get_file_name(), "");

        apply_startup_state(&window, Some(Path::new("logs/x.jsonl")));
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
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        let gpu = Arc::new(Mutex::new(Gpu {
            poller: None,
            state: new_gpu_state(TempUnit::Fahrenheit),
        }));
        handle_settings_save(
            &window,
            &app,
            &charts,
            &gpu,
            None,
            500,
            5_000,
            1000,
            "",
            "C",
            true,
            3,
            &test_update(),
        );
        assert!(!window.get_settings_visible());
        assert!(!window.get_about_visible());
        assert_eq!(gpu.lock().unwrap().state.unit, TempUnit::Celsius);
    }

    #[test]
    fn apply_gpu_unavailable_sets_empty_model() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let mut state = new_gpu_state(TempUnit::Fahrenheit);
        apply_gpu(&window, &mut state, 300_000);
        assert_eq!(window.get_gpu_devices().row_count(), 0);
    }

    #[test]
    fn apply_gpu_available_sets_one_row() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let mut state = new_gpu_state(TempUnit::Fahrenheit);
        state.snapshot = GpuSnapshot {
            available: true,
            devices: vec![gpu::GpuDevice {
                index: 0,
                name: Some("NVIDIA GPU 0".to_owned()),
                utilization: Some(80),
                memory_used: Some(512 * 1024 * 1024),
                memory_total: Some(4 * 1024 * 1024 * 1024),
                temperature: Some(60),
                memory_temperature: Some(70),
            }],
        };
        apply_gpu(&window, &mut state, 300_000);
        assert_eq!(window.get_gpu_devices().row_count(), 1);
    }

    #[test]
    fn apply_gpu_re_renders_on_unit_change() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let mut state = new_gpu_state(TempUnit::Fahrenheit);
        state.snapshot = GpuSnapshot {
            available: true,
            devices: vec![gpu::GpuDevice {
                index: 0,
                name: Some("NVIDIA GPU 0".to_owned()),
                utilization: Some(80),
                memory_used: Some(512 * 1024 * 1024),
                memory_total: Some(4 * 1024 * 1024 * 1024),
                temperature: Some(60),
                memory_temperature: Some(70),
            }],
        };
        apply_gpu(&window, &mut state, 300_000);
        // M5: the GPU rows are re-rendered whenever the frame could change,
        // so a unit change is picked up on the next apply even though the
        // readings are unchanged.
        state.unit = TempUnit::Celsius;
        apply_gpu(&window, &mut state, 300_000);
        let Some(row) = window.get_gpu_devices().row_data(0) else {
            panic!("one GPU row expected");
        };
        // 60 °C reads back as 60 with the °C unit label.
        assert_eq!(row.temp_left_value, "60");
        assert_eq!(row.temp_left_unit, "°C");
    }

    #[test]
    fn apply_gpu_skips_when_frame_unchanged() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let mut state = new_gpu_state(TempUnit::Fahrenheit);
        state.snapshot = GpuSnapshot {
            available: true,
            devices: vec![gpu::GpuDevice {
                index: 0,
                name: Some("NVIDIA GPU 0".to_owned()),
                utilization: Some(80),
                memory_used: Some(512 * 1024 * 1024),
                memory_total: Some(4 * 1024 * 1024 * 1024),
                temperature: Some(60),
                memory_temperature: Some(70),
            }],
        };
        state
            .history
            .push(gpu::sample_from_snapshot(&state.snapshot, gpu::now_ms()));
        apply_gpu(&window, &mut state, 300_000);
        let Some(k1) = state.last_gpu_key else {
            panic!("the first apply must render");
        };
        apply_gpu(&window, &mut state, 300_000);
        let k2 = state.last_gpu_key.unwrap();
        // Same data, size, window, unit: only the right-edge second may have
        // advanced between the two applies.
        assert_eq!((k2.0, k2.1, k2.2, k2.3), (k1.0, k1.1, k1.2, k1.3));
        assert!((0..=1).contains(&(k2.4 - k1.4)), "quantum {k1:?} -> {k2:?}");
        // A new poll (data epoch bump) forces a re-render on the next apply.
        state.data_epoch += 1;
        apply_gpu(&window, &mut state, 300_000);
        let k3 = state.last_gpu_key.unwrap();
        assert_eq!(k3.3, k1.3 + 1, "new data must re-render");
    }

    #[test]
    fn about_version_defaults_to_empty() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        assert_eq!(window.get_about_version(), "");
    }

    #[test]
    fn resolve_path_comes_from_config() {
        let cfg = make_config(Some("saved.jsonl"), 200, 60_000, 1_000);
        let r = resolve(&cfg);
        assert_eq!(r.path, Some(PathBuf::from("saved.jsonl")));
    }

    #[test]
    fn resolve_no_last_path_yields_no_path() {
        let r = resolve(&Config::default());
        assert_eq!(r.path, None);
    }

    #[test]
    fn resolve_log_poll_comes_from_config() {
        let cfg = make_config(None, 200, 60_000, 1_000);
        let r = resolve(&cfg);
        assert_eq!(r.log_poll_interval, Duration::from_millis(200));
    }

    #[test]
    fn resolve_config_log_poll_is_clamped() {
        let cfg = make_config(None, 999_999, 60_000, 1_000);
        let r = resolve(&cfg);
        assert_eq!(
            r.log_poll_interval,
            Duration::from_millis(config::MAX_LOG_POLL_MS)
        );
    }

    #[test]
    fn resolve_gpu_poll_comes_from_config_clamped() {
        let cfg = make_config(None, 200, 60_000, 1_000);
        let r = resolve(&cfg);
        assert_eq!(r.gpu_poll_interval_ms, config::DEFAULT_GPU_POLL_MS);

        let mut cfg = make_config(None, 200, 60_000, 1_000);
        cfg.gpu_poll_interval_ms = 999_999;
        let r = resolve(&cfg);
        assert_eq!(r.gpu_poll_interval_ms, config::MAX_GPU_POLL_MS);
    }

    #[test]
    fn resolve_window_comes_from_config_clamped() {
        let cfg = make_config(None, 200, 999_999_999, 1_000);
        let r = resolve(&cfg);
        assert_eq!(r.chart_window_ms, config::MAX_WINDOW_MS);
    }

    #[test]
    fn resolve_max_requests_comes_from_config_clamped() {
        let cfg = make_config(None, 200, 60_000, 999_999);
        let r = resolve(&cfg);
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

        handle_open_file(&window, &app, &charts, Some(&cp), path_b.clone());
        assert_eq!(window.get_file_name_full(), path_b.display().to_string());
        assert_eq!(window.get_kpi_decode_value(), "—", "the view is reset");
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

        handle_open_file(&window, &app, &charts, None, path_b.clone());
        assert_eq!(window.get_file_name_full(), path_b.display().to_string());
        assert_eq!(window.get_kpi_decode_value(), "—", "the view is reset");
    }

    #[test]
    fn settings_save_switches_pipeline_and_backfills_new_file() {
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

        // Pre-existing content in the new file: the switch must backfill it.
        append_throughput_line(&path_b, 5_000, 9.9);

        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            500,
            5_000,
            1000,
            &path_b.display().to_string(),
            "F",
            true,
            3,
            &test_update(),
        );
        assert_eq!(app.lock().unwrap().path, Some(path_b.clone()));
        assert_eq!(window.get_kpi_decode_value(), "—", "the view is reset");
        assert_eq!(
            config::load(&cp).last_path,
            Some(path_b.display().to_string())
        );

        // The backfilled line from the new file is applied.
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });
        assert_eq!(window.get_kpi_decode_value(), "9.9");
    }

    #[test]
    fn settings_save_keeps_pipeline_when_settings_unchanged() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.jsonl");
        let cp = dir.path().join("config.json");
        std::fs::File::create(&path_a).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(200),
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

        // Saving with the same file, log file poll interval, and max request
        // rows must not restart the pipeline or reset the view.
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            200,
            5_000,
            config::DEFAULT_MAX_REQUESTS as i32,
            &path_a.display().to_string(),
            "F",
            true,
            3,
            &test_update(),
        );
        assert_eq!(app.lock().unwrap().path, Some(path_a.clone()));
        assert_eq!(
            window.get_kpi_decode_value(),
            "1.5",
            "the view is not reset when the settings are unchanged"
        );
        assert!(!window.get_settings_visible(), "the dialog is closed");
    }

    #[test]
    fn settings_save_restarts_pipeline_when_poll_changes() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.jsonl");
        let cp = dir.path().join("config.json");
        std::fs::File::create(&path_a).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(200),
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

        // Changing only the log file poll interval (same file) must restart
        // the tail so the new interval takes effect immediately.
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            500,
            5_000,
            config::DEFAULT_MAX_REQUESTS as i32,
            &path_a.display().to_string(),
            "F",
            true,
            3,
            &test_update(),
        );
        assert_eq!(app.lock().unwrap().path, Some(path_a.clone()));
        assert_eq!(
            window.get_kpi_decode_value(),
            "—",
            "the view is reset on restart"
        );
        assert_eq!(
            app.lock()
                .unwrap()
                .pipeline
                .as_ref()
                .unwrap()
                .poll_interval(),
            Duration::from_millis(500),
            "the restarted pipeline uses the new log file poll interval"
        );
        assert_eq!(
            config::load(&cp).log_poll_interval_ms,
            500,
            "the new log file poll interval is persisted"
        );

        // The backfilled line is applied after the restart.
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });
        assert_eq!(window.get_kpi_decode_value(), "1.5");
    }

    #[test]
    fn settings_save_restarts_pipeline_when_max_requests_changes() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.jsonl");
        let cp = dir.path().join("config.json");
        std::fs::File::create(&path_a).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(200),
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

        // Changing only the max request rows (same file, same poll) must
        // restart the tail so the new cap takes effect immediately.
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            200,
            5_000,
            250,
            &path_a.display().to_string(),
            "F",
            true,
            3,
            &test_update(),
        );
        assert_eq!(app.lock().unwrap().path, Some(path_a.clone()));
        assert_eq!(
            window.get_kpi_decode_value(),
            "—",
            "the view is reset on restart"
        );
        assert_eq!(
            app.lock()
                .unwrap()
                .pipeline
                .as_ref()
                .unwrap()
                .max_requests(),
            250,
            "the restarted pipeline uses the new max request rows"
        );
        assert_eq!(
            config::load(&cp).max_requests,
            250,
            "the new max request rows are persisted"
        );
    }

    #[test]
    fn settings_save_ignores_blank_file() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.jsonl");
        let cp = dir.path().join("config.json");
        std::fs::File::create(&path_a).unwrap();

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

        // A blank file field keeps the current file (no switch).
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            500,
            5_000,
            1000,
            "   ",
            "F",
            true,
            3,
            &test_update(),
        );
        assert_eq!(app.lock().unwrap().path, Some(path_a.clone()));
        assert_eq!(
            window.get_kpi_decode_value(),
            "1.5",
            "the view is not reset for a blank file"
        );
    }

    #[test]
    fn settings_save_switches_to_missing_file_and_recovers() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.jsonl");
        let missing = dir.path().join("missing.jsonl");
        let cp = dir.path().join("config.json");
        std::fs::File::create(&path_a).unwrap();

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

        // Switch to a file that does not exist: the app still switches and
        // the tail reports Disconnected, retrying until the file appears.
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            500,
            5_000,
            1000,
            &missing.display().to_string(),
            "F",
            true,
            3,
            &test_update(),
        );
        assert_eq!(app.lock().unwrap().path, Some(missing.clone()));
        assert!(
            app.lock().unwrap().pipeline.is_some(),
            "a pipeline is started for the missing file"
        );
        assert_eq!(window.get_kpi_decode_value(), "—", "the view is reset");
        assert_eq!(
            config::load(&cp).last_path,
            Some(missing.display().to_string())
        );

        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_connection_status() == "Disconnected"
        });
        assert_eq!(window.get_connection_status(), "Disconnected");

        // When the file appears, the tail recovers to Live and backfills it.
        std::fs::File::create(&missing).unwrap();
        append_throughput_line(&missing, 7_000, 4.2);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_connection_status() == "Live"
        });
        assert_eq!(window.get_connection_status(), "Live");
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });
        assert_eq!(window.get_kpi_decode_value(), "4.2");
    }

    #[test]
    fn startup_start_with_valid_file_starts_pipeline_and_hides_screen() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::File::create(&path).unwrap();

        let window = MainWindow::new().unwrap();
        window.set_startup_visible(true);
        window.set_startup_file(path.display().to_string().into());
        window.set_startup_start_enabled(true);

        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        handle_startup_start(&window, &app, &charts, None);

        assert!(
            !window.get_startup_visible(),
            "the startup screen is hidden"
        );
        assert!(
            app.lock().unwrap().pipeline.is_some(),
            "the pipeline is started"
        );
        assert_eq!(app.lock().unwrap().path, Some(path.clone()));
        assert_eq!(window.get_file_name_full(), path.display().to_string());
    }

    #[test]
    fn startup_start_with_invalid_file_does_nothing() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jsonl");

        let window = MainWindow::new().unwrap();
        window.set_startup_visible(true);
        window.set_startup_file(path.display().to_string().into());
        window.set_startup_start_enabled(true);

        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        handle_startup_start(&window, &app, &charts, None);

        assert!(
            window.get_startup_visible(),
            "the startup screen stays visible"
        );
        assert!(
            app.lock().unwrap().pipeline.is_none(),
            "no pipeline is started"
        );
        assert_eq!(app.lock().unwrap().path, None);
    }

    #[test]
    fn startup_start_with_empty_file_does_nothing() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window.set_startup_visible(true);
        window.set_startup_file("".into());
        window.set_startup_start_enabled(true);

        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        handle_startup_start(&window, &app, &charts, None);

        assert!(
            window.get_startup_visible(),
            "the startup screen stays visible"
        );
        assert!(
            app.lock().unwrap().pipeline.is_none(),
            "no pipeline is started"
        );
    }

    #[test]
    fn startup_file_edited_updates_start_enabled() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::File::create(&path).unwrap();

        let window = MainWindow::new().unwrap();
        window.set_startup_visible(true);

        handle_startup_file_edited(&window, "");
        assert!(
            !window.get_startup_start_enabled(),
            "an empty path disables Start"
        );

        handle_startup_file_edited(&window, &path.display().to_string());
        assert!(
            window.get_startup_start_enabled(),
            "a valid path enables Start"
        );

        handle_startup_file_edited(&window, "missing.jsonl");
        assert!(
            !window.get_startup_start_enabled(),
            "an invalid path disables Start"
        );
    }

    #[test]
    fn window_changed_saves_config() {
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
        let gpu = test_gpu();

        handle_window_changed(&window, &app, &charts, &gpu, Some(&cp), 900_000);
        assert_eq!(charts.lock().unwrap().window_ms, 900_000);
        assert_eq!(config::load(&cp).chart_window_ms, 900_000);

        handle_window_changed(&window, &app, &charts, &gpu, Some(&cp), 60_000);
        assert_eq!(charts.lock().unwrap().window_ms, 60_000);
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
        let gpu = test_gpu();

        handle_window_changed(&window, &app, &charts, &gpu, None, 900_000);
        assert_eq!(charts.lock().unwrap().window_ms, 900_000);
    }

    #[test]
    fn render_charts_skips_when_frame_unchanged() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let mut state = new_chart_state(300_000, config::DEFAULT_MAX_REQUESTS);
        let raw = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":1000,"event":"throughput","throughput_tokens_per_second":{"decode":1.5,"prefill":2.5}}"#;
        state
            .store
            .apply(&ninfer_monitor::parser::parse_line(raw.as_bytes()).unwrap());
        render_charts(&window, &mut state);
        let Some(k1) = state.last_chart_key else {
            panic!("the first call must render");
        };
        render_charts(&window, &mut state);
        let k2 = state.last_chart_key.unwrap();
        // Same data, size, window: only the right-edge second may have
        // advanced between the two calls.
        assert_eq!((k2.0, k2.1, k2.2), (k1.0, k1.1, k1.2));
        assert!((0..=1).contains(&(k2.3 - k1.3)), "quantum {k1:?} -> {k2:?}");
        // New data (data epoch bump) forces a re-render on the next call.
        state.data_epoch += 1;
        state
            .store
            .apply(&ninfer_monitor::parser::parse_line(raw.as_bytes()).unwrap());
        render_charts(&window, &mut state);
        let k3 = state.last_chart_key.unwrap();
        assert_eq!(k3.2, k1.2 + 1, "new data must re-render");
    }

    #[test]
    fn window_changed_rerenders_gpu_rows_immediately() {
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
        let gpu = test_gpu();
        {
            let mut g = gpu.lock().unwrap();
            let snap = GpuSnapshot {
                available: true,
                devices: vec![gpu::GpuDevice {
                    index: 0,
                    name: Some("NVIDIA GPU 0".to_owned()),
                    utilization: Some(80),
                    memory_used: Some(512 * 1024 * 1024),
                    memory_total: Some(4 * 1024 * 1024 * 1024),
                    temperature: Some(60),
                    memory_temperature: Some(70),
                }],
            };
            g.state.snapshot = snap.clone();
            // A sample at the current time so the charts contain data in
            // both windows (a far-past sample would render "no data" in
            // both).
            g.state
                .history
                .push(gpu::sample_from_snapshot(&snap, gpu::now_ms()));
            apply_gpu(&window, &mut g.state, 300_000);
            drop(g);
            // The initial render must have happened.
            assert_eq!(window.get_gpu_devices().row_count(), 1);
        }
        // A window change re-renders the GPU rows at once (no poll needed):
        // the chart is rendered with the new 900 s window.
        let before = window.get_gpu_devices().row_data(0).unwrap().usage_image;
        handle_window_changed(&window, &app, &charts, &gpu, None, 900_000);
        let after = window.get_gpu_devices().row_data(0).unwrap().usage_image;
        assert_ne!(
            before, after,
            "a window change must re-render the GPU rows at once"
        );
    }

    #[test]
    fn window_changed_is_a_noop_for_gpu_when_unavailable() {
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
        let gpu = test_gpu();
        handle_window_changed(&window, &app, &charts, &gpu, None, 900_000);
        let g = gpu.lock().unwrap();
        assert!(!g.state.snapshot.available);
        assert_eq!(window.get_gpu_devices().row_count(), 0);
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
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":200,"chart_window_ms":60000}"#,
        )
        .unwrap();
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
        assert_eq!(cfg.log_poll_interval_ms, 200, "the other settings are kept");
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
    fn column_changed_moves_both_columns_and_updates_the_ui() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let cols = Arc::new(Mutex::new(ColumnWidths::default()));

        // The divider between Id and Time: both columns resize, the total
        // stays constant, the other columns are untouched.
        handle_column_changed(&window, &cols, 0, 60.0);
        assert_eq!(window.get_col_id(), 60.0);
        assert_eq!(window.get_col_time(), 85.0);
        assert_eq!(
            window.get_col_model(),
            456.0,
            "the other columns are untouched"
        );
        let c = cols.lock().unwrap();
        assert_eq!(c.width(Column::Id), 60);
        assert_eq!(c.width(Column::Time), 85);
        assert_eq!(c.total(), ColumnWidths::default().total());
    }

    #[test]
    fn column_changed_clamps_at_the_minimums() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let cols = Arc::new(Mutex::new(ColumnWidths::default()));

        // Drag past the left minimum: Id stays at its minimum, Time grows.
        handle_column_changed(&window, &cols, 0, -1_000.0);
        assert_eq!(
            window.get_col_id(),
            ninfer_monitor::columns::MIN_WIDTHS[Column::Id.index()] as f32
        );
        assert_eq!(
            window.get_col_time(),
            40.0 + 105.0 - ninfer_monitor::columns::MIN_WIDTHS[Column::Id.index()] as f32
        );

        // Drag past the right minimum from the clamped state: Time stays at
        // its minimum, Id grows.
        handle_column_changed(&window, &cols, 0, 10_000.0);
        assert_eq!(
            window.get_col_time(),
            ninfer_monitor::columns::MIN_WIDTHS[Column::Time.index()] as f32
        );
        let c = cols.lock().unwrap();
        assert_eq!(c.width(Column::Id), 32 + 113 - 85);
        assert_eq!(
            c.total(),
            ColumnWidths::default().total(),
            "the total stays constant"
        );
    }

    #[test]
    fn column_changed_ignores_out_of_range_sliders() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let cols = Arc::new(Mutex::new(ColumnWidths::default()));

        handle_column_changed(&window, &cols, 9, 60.0);
        handle_column_changed(&window, &cols, -1, 60.0);
        assert_eq!(
            window.get_col_id(),
            40.0,
            "an out-of-range slider is ignored"
        );
        assert_eq!(window.get_col_time(), 105.0);
        assert_eq!(
            *cols.lock().unwrap(),
            ColumnWidths::default(),
            "the state is untouched"
        );
    }

    #[test]
    fn column_changed_works_with_the_flex_model_column() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let cols = Arc::new(Mutex::new(ColumnWidths::default()));

        // The divider between Model and Prompt: the Model column absorbs.
        handle_column_changed(&window, &cols, 2, 356.0);
        assert_eq!(window.get_col_model(), 356.0);
        assert_eq!(window.get_col_prompt(), 60.0 + (456.0 - 356.0));
        let c = cols.lock().unwrap();
        assert_eq!(c.total(), ColumnWidths::default().total());
    }

    #[test]
    fn column_released_persists_widths_and_keeps_other_settings() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":200,"chart_window_ms":60000}"#,
        )
        .unwrap();
        let window = MainWindow::new().unwrap();
        let cols = Arc::new(Mutex::new(ColumnWidths::default()));

        handle_column_changed(&window, &cols, 0, 60.0);
        handle_column_released(&window, Some(&cp), &cols);

        let expected_width = logical_window_width(&window);
        let cfg = config::load(&cp);
        let saved = cfg.table_columns.expect("table_columns is persisted");
        assert_eq!(saved.window_width, expected_width);
        assert_eq!(saved.id, 60);
        assert_eq!(saved.time, 85);
        assert_eq!(saved.model, 456);
        assert_eq!(cfg.log_poll_interval_ms, 200, "the other settings are kept");
        assert_eq!(cfg.chart_window_ms, 60_000);
    }

    #[test]
    fn column_released_without_config_is_a_noop() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let cols = Arc::new(Mutex::new(ColumnWidths::default()));
        handle_column_changed(&window, &cols, 0, 60.0);
        handle_column_released(&window, None, &cols);
        assert_eq!(
            cols.lock().unwrap().width(Column::Id),
            60,
            "the state is kept"
        );
    }

    #[test]
    fn tick_columns_rebalances_the_model_column_to_the_window_width() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window
            .window()
            .set_size(slint::WindowSize::Logical(slint::LogicalSize::new(
                1280.0, 800.0,
            )));
        let cols = Arc::new(Mutex::new(ColumnWidths::default()));

        tick_columns(&window, &cols);
        let model_at_default = cols.lock().unwrap().width(Column::Model);
        assert_eq!(
            model_at_default, 456,
            "the default widths fill the 1280px window"
        );

        // A 128px narrower window shrinks the Model column (the flex column)
        // by exactly 128px.
        window
            .window()
            .set_size(slint::WindowSize::Logical(slint::LogicalSize::new(
                logical_window_width(&window) - 128.0,
                800.0,
            )));
        tick_columns(&window, &cols);
        let c = cols.lock().unwrap();
        assert_eq!(
            c.width(Column::Model),
            model_at_default - 128,
            "the Model column absorbs the window resize"
        );
        assert_eq!(c.width(Column::Id), 40, "the fixed columns are untouched");
        assert_eq!(
            window.get_col_model(),
            (model_at_default - 128) as f32,
            "the UI follows"
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
    fn settings_open_prefills_from_config_and_current_file() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":200,"gpu_poll_interval_ms":3000,"chart_window_ms":60000,"max_requests":250,"update_check_enabled":false,"update_check_period_days":9}"#,
        )
        .unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: Some(PathBuf::from("logs/x.jsonl")),
        }));
        let update = test_update();
        {
            let mut u = update.lock().unwrap();
            u.enabled = false;
            u.period_days = 9;
        }
        handle_settings_open(&window, &app, &update, Some(&cp));
        assert!(window.get_settings_visible());
        assert_eq!(window.get_settings_log_poll_ms(), 200);
        assert_eq!(window.get_settings_gpu_poll_ms(), 3_000);
        assert_eq!(window.get_settings_max_requests(), 250);
        assert_eq!(window.get_settings_file(), "logs/x.jsonl");
        assert!(!window.get_settings_update_check_enabled());
        assert_eq!(window.get_settings_update_check_days(), 9);
    }

    #[test]
    fn settings_open_without_config_uses_defaults() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        handle_settings_open(&window, &app, &test_update(), None);
        assert!(window.get_settings_visible());
        assert_eq!(
            window.get_settings_log_poll_ms(),
            Config::default().log_poll_interval_ms as i32
        );
        assert_eq!(
            window.get_settings_gpu_poll_ms(),
            config::DEFAULT_GPU_POLL_MS as i32
        );
        assert_eq!(
            window.get_settings_max_requests(),
            config::DEFAULT_MAX_REQUESTS as i32
        );
        assert_eq!(window.get_settings_file(), "");
        assert!(window.get_settings_update_check_enabled());
        assert_eq!(
            window.get_settings_update_check_days(),
            update_check::DEFAULT_PERIOD_DAYS as i32
        );
    }

    #[test]
    fn settings_save_persists_poll_and_max_requests() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":500,"gpu_poll_interval_ms":5000,"chart_window_ms":300000}"#,
        )
        .unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            200,
            2_000,
            250,
            "",
            "F",
            true,
            3,
            &test_update(),
        );
        assert!(!window.get_settings_visible());
        let cfg = config::load(&cp);
        assert_eq!(cfg.log_poll_interval_ms, 200);
        assert_eq!(cfg.gpu_poll_interval_ms, 2_000);
        assert_eq!(cfg.max_requests, 250);
        assert_eq!(cfg.chart_window_ms, 300_000);
        assert_eq!(cfg.temp_unit, "F");
    }

    #[test]
    fn settings_save_clamps_out_of_range() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":500,"chart_window_ms":300000}"#,
        )
        .unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            1,
            999_999,
            999_999,
            "",
            "C",
            true,
            3,
            &test_update(),
        );
        let cfg = config::load(&cp);
        assert_eq!(cfg.log_poll_interval_ms, config::MIN_LOG_POLL_MS);
        assert_eq!(cfg.gpu_poll_interval_ms, config::MAX_GPU_POLL_MS);
        assert_eq!(cfg.max_requests, config::MAX_MAX_REQUESTS);
        assert_eq!(cfg.temp_unit, "C");
    }

    #[test]
    fn settings_save_applies_gpu_poll_interval_to_poller() {
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
        let gpu = Arc::new(Mutex::new(Gpu {
            poller: Some(GpuPoller::start(5_000)),
            state: new_gpu_state(TempUnit::Fahrenheit),
        }));

        handle_settings_save(
            &window,
            &app,
            &charts,
            &gpu,
            None,
            500,
            2_000,
            1000,
            "",
            "F",
            true,
            3,
            &test_update(),
        );
        assert_eq!(
            gpu.lock()
                .unwrap()
                .poller
                .as_ref()
                .unwrap()
                .poll_interval_ms(),
            2_000,
            "the GPU poller uses the new GPU poll interval"
        );
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

    #[test]
    fn startup_config_clamps_out_of_range_update_period() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"update_check_period_days":99}"#).unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(
            cfg.update_check_period_days,
            update_check::MAX_PERIOD_DAYS,
            "the stored period is clamped"
        );
        assert_eq!(
            config::load(&cp).update_check_period_days,
            update_check::MAX_PERIOD_DAYS,
            "the clamped period is written back"
        );
        std::fs::write(&cp, r#"{"update_check_period_days":0}"#).unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(cfg.update_check_period_days, update_check::MIN_PERIOD_DAYS);
    }

    fn test_release() -> LatestRelease {
        LatestRelease {
            tag: "v0.3.0".to_owned(),
            version: "0.3.0".to_owned(),
            url: "https://github.com/lsh123/ninfer-monitor/releases/tag/v0.3.0".to_owned(),
            assets: Vec::new(),
        }
    }

    #[test]
    fn update_check_success_persists_last_check_and_sets_available() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"log_poll_interval_ms":200}"#).unwrap();
        let update = test_update();
        let now = 1_700_000_000_000u64;
        assert!(run_update_check_with(&update, Some(&cp), now, || {
            Ok(Some(test_release()))
        }));
        let cfg = config::load(&cp);
        assert_eq!(cfg.last_update_check_unix_ms, Some(now));
        assert_eq!(cfg.log_poll_interval_ms, 200, "the other settings are kept");
        let u = update.lock().unwrap();
        assert_eq!(u.last_check_unix_ms, Some(now));
        assert_eq!(
            u.available.as_ref().map(|r| r.version.as_str()),
            Some("0.3.0")
        );
    }

    #[test]
    fn update_check_success_without_new_release_clears_available() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        let update = Arc::new(Mutex::new(UpdateState {
            enabled: true,
            period_days: 3,
            last_check_unix_ms: None,
            last_attempt_unix_ms: None,
            available: Some(test_release()),
        }));
        let now = 1_700_000_000_000u64;
        assert!(run_update_check_with(&update, Some(&cp), now, || Ok(None)));
        let u = update.lock().unwrap();
        assert_eq!(u.last_check_unix_ms, Some(now));
        assert!(u.available.is_none(), "no newer release -> not available");
        assert_eq!(config::load(&cp).last_update_check_unix_ms, Some(now));
    }

    #[test]
    fn update_check_failure_keeps_state_and_is_throttled() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        let update = test_update();
        let now = 1_700_000_000_000u64;
        let error = || Err(update_check::CheckError::Http("timeout".to_owned()));
        assert!(
            run_update_check_with(&update, Some(&cp), now, error),
            "a due check is attempted"
        );
        let u = update.lock().unwrap();
        assert_eq!(
            u.last_check_unix_ms, None,
            "a failed check does not record a check"
        );
        assert!(u.available.is_none());
        assert!(
            config::load(&cp).last_update_check_unix_ms.is_none(),
            "nothing persisted"
        );
        drop(u);
        assert!(
            !run_update_check_with(
                &update,
                Some(&cp),
                now + UPDATE_CHECK_RETRY_AFTER_MS - 1,
                error
            ),
            "a retry within the throttle window is skipped"
        );
        assert!(
            run_update_check_with(&update, Some(&cp), now + UPDATE_CHECK_RETRY_AFTER_MS, error),
            "the next scheduled check retries after the throttle window"
        );
        assert_eq!(
            update.lock().unwrap().last_check_unix_ms,
            None,
            "the repeated failures still do not record a check"
        );
    }

    #[test]
    fn update_check_disabled_never_runs() {
        let update = Arc::new(Mutex::new(UpdateState {
            enabled: false,
            period_days: 3,
            last_check_unix_ms: None,
            last_attempt_unix_ms: None,
            available: Some(test_release()),
        }));
        let calls = std::cell::Cell::new(0);
        assert!(!run_update_check_with(
            &update,
            None,
            1_700_000_000_000,
            || {
                calls.set(calls.get() + 1);
                Ok(Some(test_release()))
            }
        ));
        assert_eq!(calls.get(), 0, "the checker is not called when disabled");
        assert!(
            update.lock().unwrap().available.is_some(),
            "disabling does not clear a found update by itself"
        );
    }

    #[test]
    fn update_check_disabled_mid_flight_does_not_show_available() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        let update = test_update();
        let now = 1_700_000_000_000u64;
        assert!(run_update_check_with(&update, Some(&cp), now, || {
            {
                let mut u = update.lock().unwrap();
                u.enabled = false;
                u.available = None;
            }
            Ok(Some(test_release()))
        }));
        let u = update.lock().unwrap();
        assert_eq!(
            u.last_check_unix_ms,
            Some(now),
            "the successful check time is still recorded"
        );
        assert!(
            u.available.is_none(),
            "disabling mid-flight must not (re-)show the update button"
        );
        assert_eq!(
            config::load(&cp).last_update_check_unix_ms,
            Some(now),
            "the check time is persisted anyway"
        );
    }

    #[test]
    fn update_check_not_due_never_runs() {
        let now = 1_700_000_000_000u64;
        let update = Arc::new(Mutex::new(UpdateState {
            enabled: true,
            period_days: 3,
            last_check_unix_ms: Some(now),
            last_attempt_unix_ms: None,
            available: None,
        }));
        assert!(!run_update_check_with(
            &update,
            None,
            now + 3_600_000,
            || Ok(Some(test_release()))
        ));
        assert!(update.lock().unwrap().available.is_none());
    }

    #[test]
    fn update_check_without_config_path_does_not_persist() {
        let update = test_update();
        let now = 1_700_000_000_000u64;
        assert!(run_update_check_with(&update, None, now, || Ok(Some(
            test_release()
        ))));
        assert_eq!(update.lock().unwrap().last_check_unix_ms, Some(now));
    }

    #[test]
    fn persist_last_update_check_keeps_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"log_poll_interval_ms":200,"max_requests":250}"#).unwrap();
        persist_last_update_check(Some(&cp), 1_700_000_000_000);
        let cfg = config::load(&cp);
        assert_eq!(cfg.last_update_check_unix_ms, Some(1_700_000_000_000));
        assert_eq!(cfg.log_poll_interval_ms, 200);
        assert_eq!(cfg.max_requests, 250);
        persist_last_update_check(None, 1);
    }

    #[test]
    fn settings_open_prefills_update_check_settings() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"update_check_enabled":false,"update_check_period_days":5}"#,
        )
        .unwrap();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let update = test_update();
        {
            let mut u = update.lock().unwrap();
            u.enabled = false;
            u.period_days = 5;
        }
        handle_settings_open(&window, &app, &update, Some(&cp));
        assert!(!window.get_settings_update_check_enabled());
        assert_eq!(window.get_settings_update_check_days(), 5);
    }

    #[test]
    fn settings_save_persists_update_check_settings() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        let update = test_update();
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            500,
            5_000,
            1000,
            "",
            "F",
            false,
            7,
            &update,
        );
        let cfg = config::load(&cp);
        assert!(!cfg.update_check_enabled);
        assert_eq!(cfg.update_check_period_days, 7);
        let u = update.lock().unwrap();
        assert!(!u.enabled);
        assert_eq!(u.period_days, 7);
    }

    #[test]
    fn settings_save_clamps_update_check_period() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        let update = test_update();
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            500,
            5_000,
            1000,
            "",
            "F",
            true,
            99,
            &update,
        );
        let cfg = config::load(&cp);
        assert!(cfg.update_check_enabled);
        assert_eq!(
            cfg.update_check_period_days,
            update_check::MAX_PERIOD_DAYS,
            "an out-of-range period is clamped before persisting"
        );
    }

    #[test]
    fn settings_save_disable_clears_update_available() {
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
        let update = test_update();
        apply_update_available(&window, Some(&test_release()));
        assert!(window.get_update_available());
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            None,
            500,
            5_000,
            1000,
            "",
            "F",
            false,
            3,
            &update,
        );
        assert!(
            !window.get_update_available(),
            "disabling the check hides the update button"
        );
        assert!(
            update.lock().unwrap().available.is_none(),
            "disabling the check clears the found update"
        );
    }

    #[test]
    fn update_button_click_shows_dialog_and_close_hides_it() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        assert!(!window.get_update_visible());
        handle_update_clicked(&window);
        assert!(window.get_update_visible());
        handle_update_close(&window);
        assert!(!window.get_update_visible());
    }

    #[test]
    fn update_install_without_a_found_release_does_nothing() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let update = test_update();
        let in_flight = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let pending: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        handle_update_install(
            &window,
            &update,
            &in_flight,
            &cancel,
            &pending,
            &Arc::new(Mutex::new(None)),
            None,
        );
        assert!(
            !in_flight.load(Ordering::SeqCst),
            "no download may be in flight"
        );
        assert!(!window.get_update_downloading(), "no download state is set");
        assert!(pending.lock().unwrap().is_none());
    }

    #[test]
    fn update_install_ignores_reclicks_while_in_flight() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let update = test_update();
        update.lock().unwrap().available = Some(test_release());
        let in_flight = Arc::new(AtomicBool::new(true));
        let cancel = Arc::new(AtomicBool::new(false));
        let pending: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        handle_update_install(
            &window,
            &update,
            &in_flight,
            &cancel,
            &pending,
            &Arc::new(Mutex::new(None)),
            None,
        );
        assert!(
            !window.get_update_downloading(),
            "a second click must not start another download"
        );
        assert!(pending.lock().unwrap().is_none());
    }

    #[test]
    fn update_install_cancel_removes_the_installer_and_resets_state() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window.set_update_downloading(true);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installer.exe");
        std::fs::write(&path, b"partial").unwrap();
        let pending: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let should_quit = apply_install_result(
            &window,
            Ok(path.clone()),
            true,
            &pending,
            &Arc::new(Mutex::new(None)),
            None,
        );
        assert!(!should_quit, "a canceled download must not quit the loop");
        assert!(!path.exists(), "a canceled download removes the file");
        assert!(
            !window.get_update_downloading(),
            "the download state is cleared"
        );
        assert_eq!(window.get_update_download_error(), "", "no error is shown");
        assert!(pending.lock().unwrap().is_none(), "no installer is queued");
    }

    #[test]
    fn update_install_success_keeps_the_installer_and_quits() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installer.exe");
        std::fs::write(&path, b"installer").unwrap();
        let pending: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let should_quit = apply_install_result(
            &window,
            Ok(path.clone()),
            false,
            &pending,
            &Arc::new(Mutex::new(None)),
            None,
        );
        assert!(should_quit, "a confirmed update quits the event loop");
        assert!(path.exists(), "the installer file is kept");
        assert_eq!(
            pending.lock().unwrap().as_deref(),
            Some(path.as_path()),
            "the installer path is queued for the post-event-loop launch"
        );
    }

    #[test]
    fn update_install_error_shows_the_error_and_keeps_the_dialog() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window.set_update_downloading(true);
        let pending: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let should_quit = apply_install_result(
            &window,
            Err(update_check::InstallError::NoAsset("Windows")),
            false,
            &pending,
            &Arc::new(Mutex::new(None)),
            None,
        );
        assert!(!should_quit);
        assert!(!window.get_update_downloading());
        assert_eq!(
            window.get_update_download_error(),
            "this release has no installer for Windows"
        );
        assert!(pending.lock().unwrap().is_none());
    }

    #[test]
    fn apply_update_available_sets_the_dialog_properties() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        apply_update_available(&window, Some(&test_release()));
        assert!(window.get_update_available());
        assert_eq!(window.get_update_version(), "0.3.0");
        assert_eq!(
            window.get_update_release_url(),
            "https://github.com/lsh123/ninfer-monitor/releases/tag/v0.3.0"
        );
        apply_update_available(&window, None);
        assert!(!window.get_update_available());
    }

    #[test]
    fn update_checker_stop_is_prompt() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let update = Arc::new(Mutex::new(UpdateState {
            enabled: false,
            period_days: 3,
            last_check_unix_ms: None,
            last_attempt_unix_ms: None,
            available: None,
        }));
        let weak = window.as_weak();
        let checker = UpdateChecker::start(update, None, weak);
        std::thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        checker.stop();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a stop is honored within the sleep granularity"
        );
    }
}
