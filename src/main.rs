// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

#![windows_subsystem = "windows"]

mod ui;

slint::include_modules!();

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ninfer_monitor::chart;
use ninfer_monitor::columns::{self, Column, ColumnWidths, TableColumns};
use ninfer_monitor::config::{self, Config, ConfigEntry};
use ninfer_monitor::config_import;
use ninfer_monitor::configs;
use ninfer_monitor::gpu::{self, GpuHistory, GpuPoller, GpuSnapshot, TempUnit};
use ninfer_monitor::kpi;
use ninfer_monitor::nvidia_popup;
use ninfer_monitor::pipeline::{Pipeline, StoreUpdate, format_last_event, shorten_file_name};
use ninfer_monitor::process_control;
use ninfer_monitor::requests;
use ninfer_monitor::server_info;
use ninfer_monitor::split::SplitPanels;
use ninfer_monitor::startup;
use ninfer_monitor::store::Store;
use ninfer_monitor::tail::TailStatus;
use ninfer_monitor::update_check::{self, LatestRelease};
use ninfer_monitor::window_state::{self, RestorePlan, WindowState};
use slint::ComponentHandle;
use slint::private_unstable_api::re_exports::ColorScheme;
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

/// A registered NInfer configuration held in memory (v0.3 M3). `id` is a
/// session-unique counter (not persisted) and the key of the tracked child
/// process (v0.3 M4); `running` reflects a process started by this instance
/// (not persisted, PRD §4.4); `error` holds the last start/exit failure so
/// the Control-tab list can mark the configuration red.
struct ControlConfig {
    id: u64,
    name: String,
    command_line: String,
    running: bool,
    error: Option<String>,
}

/// The tail/parse pipeline and the file it tails, shared between the event
/// loop and the control handlers (FR-7.1). `install_folder` is the NInfer
/// installation folder shown in the status bar (v0.3 M2); `path` is the
/// app-managed log file the pipeline tails. `configs` / `selected_config` /
/// `next_config_id` / `control_status` hold the Control-tab state (v0.3 M3);
/// `processes` tracks the child processes started by this instance, keyed by
/// configuration id (v0.3 M4; not persisted).
struct App {
    pipeline: Option<Pipeline>,
    path: Option<PathBuf>,
    install_folder: Option<PathBuf>,
    configs: Vec<ControlConfig>,
    selected_config: Option<usize>,
    next_config_id: u64,
    control_status: String,
    processes: std::collections::HashMap<u64, TrackedProcess>,
}

/// The amount of child-process console output kept for a failed start or an
/// abnormal exit (the tail of the combined stdout/stderr stream).
const PROCESS_OUTPUT_CAP: usize = 64 * 1024;

/// A tracked child process (v0.3 M4): the `Child` handle and the bounded tail
/// of its console output. The stdout/stderr reader threads that drain the
/// pipes (so the child never blocks on a full pipe buffer) are detached —
/// their `JoinHandle`s are dropped and they are never joined on the UI thread:
/// a grandchild that outlives the parent and holds a pipe open would otherwise
/// block a join indefinitely and freeze the UI.
struct TrackedProcess {
    child: std::process::Child,
    output: Arc<Mutex<Vec<u8>>>,
}

impl TrackedProcess {
    fn new(mut child: std::process::Child) -> Self {
        let output = Arc::new(Mutex::new(Vec::new()));
        if let Some(stdout) = child.stdout.take() {
            let out = output.clone();
            std::thread::spawn(move || drain_process_output(stdout, out));
        }
        if let Some(stderr) = child.stderr.take() {
            let out = output.clone();
            std::thread::spawn(move || drain_process_output(stderr, out));
        }
        Self { child, output }
    }

    fn output_text(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }
}

fn drain_process_output<R>(mut reader: R, output: Arc<Mutex<Vec<u8>>>)
where
    R: Read + Send + 'static,
{
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                let mut tail = output.lock().unwrap();
                tail.extend_from_slice(&buffer[..n]);
                let excess = tail.len().saturating_sub(PROCESS_OUTPUT_CAP);
                if excess > 0 {
                    tail.drain(..excess);
                }
            }
            Err(_) => break,
        }
    }
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

/// Pick a folder with the native dialog (v0.3 M2): used for the NInfer
/// installation folder and the log file folder.
fn pick_folder() -> Option<PathBuf> {
    rfd::FileDialog::new().pick_folder()
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
    /// The NInfer installation folder (v0.3 M2); takes precedence over the
    /// configured `install_folder` and is not persisted unless the user
    /// presses Start or Save.
    install_folder: Option<PathBuf>,
    nvidia_profile: Option<NvidiaProfileAction>,
}

fn parse_cli_args(args: &[String]) -> CliArgs {
    let mut config_path = None;
    let mut install_folder = None;
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
            // The positional argument is the NInfer installation folder
            // (v0.3 M2); the last one wins.
            other => install_folder = Some(PathBuf::from(other)),
        }
        i += 1;
    }
    CliArgs {
        config_path,
        install_folder,
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

/// Effective startup settings (FR-1.1/FR-7.2): the NInfer installation
/// folder, the log file folder, the log file poll interval, the GPU poll
/// interval, the chart window, and the max request rows all come from the
/// config.
struct Resolved {
    install_folder: Option<PathBuf>,
    log_file_folder: String,
    log_poll_interval: Duration,
    gpu_poll_interval_ms: u64,
    chart_window_ms: u64,
    max_requests: u64,
}

fn resolve(cfg: &Config) -> Resolved {
    Resolved {
        install_folder: cfg.install_folder.clone().map(PathBuf::from),
        log_file_folder: cfg.log_file_folder.clone(),
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
    // A `--config` value that is an existing directory is treated as the
    // directory holding the config, so `--config c:\tmp` uses
    // `c:\tmp\config.json` (matching the default `<home>/.ninfer-monitor/`
    // layout); any other value is the config file path itself.
    let config_path = match cli_config_path {
        Some(p) if p.is_dir() => Some(p.join(config::CONFIG_FILE)),
        Some(p) => Some(PathBuf::from(p)),
        None => config::default_config_path(),
    };
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
    let clamped_control_split = cfg.control_split();
    let stale_running_config = cfg.running_config.as_deref().is_some_and(|name| {
        !cfg.configs
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(name))
    });
    let changed = clamped_log_poll != cfg.log_poll_interval_ms
        || clamped_gpu_poll != cfg.gpu_poll_interval_ms
        || clamped_window != cfg.chart_window_ms
        || clamped_max_requests != cfg.max_requests
        || clamped_update_period != cfg.update_check_period_days
        || clamped_split != cfg.split_panels
        || clamped_control_split != cfg.control_split
        || stale_running_config;
    cfg.log_poll_interval_ms = clamped_log_poll;
    cfg.gpu_poll_interval_ms = clamped_gpu_poll;
    cfg.chart_window_ms = clamped_window;
    cfg.max_requests = clamped_max_requests;
    cfg.update_check_period_days = clamped_update_period;
    cfg.split_panels = clamped_split;
    cfg.control_split = clamped_control_split;
    if stale_running_config {
        cfg.running_config = None;
    }
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

/// Set the status-bar NInfer installation folder (PRD v0.3 §3.7, M2): the
/// full name for width measurement, plus the display name — the full name
/// when it fits the available space, otherwise the shortened form.
fn set_install_folder(window: &MainWindow, folder: &Path) {
    let name = folder.display().to_string();
    window.set_install_folder_full(name.clone().into());
    window.set_install_folder(pick_install_folder_name(window, &name).into());
}

/// Re-pick the display name from the UI's last layout pass (PRD v0.3 §3.7): the
/// available width changes with the window size, and the measured widths
/// only update after a layout pass. Called on every tick so a resize while
/// the log is idle is picked up within one tick.
fn refresh_install_folder(window: &MainWindow, app: &Arc<Mutex<App>>) {
    let Some(folder) = app.lock().unwrap().install_folder.clone() else {
        return;
    };
    let name = folder.display().to_string();
    let chosen = pick_install_folder_name(window, &name);
    if window.get_install_folder().as_str() != chosen {
        window.set_install_folder(chosen.into());
    }
}

/// The display name for `name` (PRD v0.3 §3.7): the full name when it fits the
/// space measured by the UI, otherwise the shortened form. A zero width
/// (no layout pass yet) optimistically shows the full name; the next tick
/// re-picks once the widths are known.
fn pick_install_folder_name(window: &MainWindow, name: &str) -> String {
    let full_width = window.get_install_folder_full_width();
    let available = window.get_install_folder_available();
    if full_width > 0.0 && available > 0.0 && full_width > available {
        shorten_file_name(name)
    } else {
        name.to_string()
    }
}

/// Apply the startup UI state (FR-7.1): show the NInfer installation folder
/// in the status bar when monitoring starts (v0.3 M2).
fn apply_startup_state(window: &MainWindow, install_folder: Option<&Path>) {
    if let Some(folder) = install_folder {
        set_install_folder(window, folder);
    }
}

/// Refresh the status bar fields (PRD v0.3 §3.7): the NInfer installation folder
/// (v0.3 M2), the last event time (FR-1.7), and the error/skipped counters
/// (FR-2.3/FR-3.1).
fn apply_status_bar(window: &MainWindow, install_folder: &Path, update: &StoreUpdate) {
    set_install_folder(window, install_folder);
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

/// Log file poll interval and max request rows for a pipeline started from
/// the startup screen or the Settings tab (FR-7.1/FR-7.3, v0.3 M2): the
/// (possibly externally edited) config values are re-read.
fn settings_for_new_file(config_path: Option<&Path>) -> (Duration, u64) {
    let cfg = config_path.map(config::load).unwrap_or_default();
    (cfg.log_poll_interval(), cfg.max_requests())
}

/// Persist the NInfer installation folder as `install_folder` (v0.3 M2),
/// keeping the other settings.
fn persist_install_folder(config_path: Option<&Path>, folder: &Path) {
    let Some(cp) = config_path else {
        return;
    };
    let _write = config::write_lock();
    let new_folder = folder.display().to_string();
    let mut cfg = config::load(cp);
    if cfg.install_folder.as_deref() == Some(new_folder.as_str()) {
        return;
    }
    cfg.install_folder = Some(new_folder);
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
    let (update, install_folder) = {
        let app = app.lock().unwrap();
        let update = app.pipeline.as_ref().and_then(|p| p.take_latest());
        (update, app.install_folder.clone())
    };
    let Some(update) = update else {
        return false;
    };
    let Some(install_folder) = install_folder else {
        return false;
    };
    let mut state = charts.lock().unwrap();
    apply_status_bar(window, &install_folder, &update);
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

/// Control-tab split drag handler: update the in-memory fraction (clamped)
/// and push it to the UI so the list/edit heights update live. Persistence
/// happens on release (`handle_control_split_released`).
fn handle_control_split_changed(window: &MainWindow, control_split: &Mutex<f32>, frac: f32) {
    let updated = {
        let mut s = control_split.lock().unwrap();
        *s = frac.clamp(config::MIN_CONTROL_SPLIT, config::MAX_CONTROL_SPLIT);
        *s
    };
    window.set_control_split_frac(updated);
}

/// Control-tab split release handler: persist the current fraction as
/// `control_split`, keeping the other settings.
fn handle_control_split_released(config_path: Option<&Path>, control_split: &Mutex<f32>) {
    let s = *control_split.lock().unwrap();
    if let Some(cp) = config_path {
        let _write = config::write_lock();
        let mut cfg = config::load(cp);
        if cfg.control_split != s {
            cfg.control_split = s;
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

/// Pre-fill the Settings tab fields (M1) with the current log file poll
/// interval, GPU poll interval, max request rows, the update-check settings
/// (M8), the NInfer installation folder (the tailed one when set, e.g. from
/// the CLI, otherwise the configured one), and the log file folder (v0.3
/// M2), clearing the last NVIDIA popup status. Also re-computes the Save
/// button's enabled state. Called when the Settings tab is activated and on
/// Discard (to revert the edits made in the tab).
fn prefill_settings(
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
    let install_folder = app
        .lock()
        .unwrap()
        .install_folder
        .as_deref()
        .map(|p| p.display().to_string())
        .or_else(|| cfg.install_folder.clone())
        .unwrap_or_default();
    window.set_settings_install_folder(install_folder.clone().into());
    window.set_settings_log_file_folder(cfg.log_file_folder.clone().into());
    window.set_nvidia_popup_status(String::new().into());
    window.set_settings_import_status(String::new().into());
    window.set_settings_import_enabled(settings_import_enabled(&install_folder));
    window.set_settings_save_enabled(settings_save_enabled(&install_folder, &cfg.log_file_folder));
}

/// Whether the Settings tab's Save button is enabled (v0.3 M2): the install
/// folder must contain the `ninfer-serve` executable and the log file folder
/// must be an existing directory.
fn settings_save_enabled(install_folder: &str, log_file_folder: &str) -> bool {
    startup::is_valid_install_folder(Path::new(install_folder))
        && Path::new(log_file_folder).is_dir()
}

/// Whether the Settings tab's "Import Configurations" button is enabled
/// (v0.3): the install folder field must name an existing directory.
fn settings_import_enabled(install_folder: &str) -> bool {
    let folder = install_folder.trim();
    !folder.is_empty() && Path::new(folder).is_dir()
}

/// Tab-changed handler (M1): switch to the active tab. When switching to the
/// Settings tab from a different tab, pre-fill its fields from the current
/// settings (the v0.2 dialog-open semantics); clicking the already-active
/// Settings tab is a no-op so the user's in-progress edits are kept. The tab
/// state is not persisted.
fn handle_tab_changed(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    update: &Arc<Mutex<UpdateState>>,
    config_path: Option<&Path>,
    which: i32,
) {
    let previous = window.get_active_tab();
    if previous == 0 && which != 0 {
        // Discard a pending focus request: the panel is destroyed, and the
        // recreated panel must not re-fire it with the stale counter.
        window.set_control_focus_name(0);
        let mut a = app.lock().unwrap();
        if let Some(status) = commit_pending_edit(window, &mut a, config_path) {
            a.control_status = status;
        }
    }
    window.set_active_tab(which);
    if which == 2 && previous != 2 {
        prefill_settings(window, app, update, config_path);
    }
    if which == 0 && previous != 0 {
        prefill_control(window, app);
    }
}

/// Map the persisted configuration entries to the in-memory Control-tab state
/// (v0.3 M3), assigning session-unique ids; returns the configs and the next
/// id to hand out.
fn load_control_configs(entries: &[ConfigEntry]) -> (Vec<ControlConfig>, u64) {
    let configs = entries
        .iter()
        .enumerate()
        .map(|(i, e)| ControlConfig {
            id: (i as u64) + 1,
            name: e.name.clone(),
            command_line: e.command_line.clone(),
            running: false,
            error: None,
        })
        .collect();
    let next_config_id = entries.len() as u64 + 1;
    (configs, next_config_id)
}

/// Persist the in-memory configurations to the config file (v0.3 M3),
/// preserving the other fields (load-modify-save under the write lock).
fn persist_configs(app: &App, config_path: Option<&Path>) {
    let Some(cp) = config_path else {
        return;
    };
    let _write = config::write_lock();
    let mut cfg = config::load(cp);
    cfg.configs = app
        .configs
        .iter()
        .map(|c| ConfigEntry {
            name: c.name.clone(),
            command_line: c.command_line.clone(),
        })
        .collect();
    let _ = config::save(cp, &cfg);
}

fn set_running_config(config_path: Option<&Path>, name: &str) {
    let Some(cp) = config_path else {
        return;
    };
    let _write = config::write_lock();
    let mut cfg = config::load(cp);
    cfg.running_config = Some(name.to_owned());
    let _ = config::save(cp, &cfg);
}

fn clear_running_config_if(config_path: Option<&Path>, name: &str) {
    let Some(cp) = config_path else {
        return;
    };
    let _write = config::write_lock();
    let mut cfg = config::load(cp);
    if cfg
        .running_config
        .as_deref()
        .is_some_and(|current| current.eq_ignore_ascii_case(name))
    {
        cfg.running_config = None;
        let _ = config::save(cp, &cfg);
    }
}

fn rename_running_config_if(config_path: Option<&Path>, old_name: &str, new_name: &str) {
    let Some(cp) = config_path else {
        return;
    };
    let _write = config::write_lock();
    let mut cfg = config::load(cp);
    if cfg
        .running_config
        .as_deref()
        .is_some_and(|current| current.eq_ignore_ascii_case(old_name))
    {
        cfg.running_config = Some(new_name.to_owned());
        let _ = config::save(cp, &cfg);
    }
}

/// Returns `true` when a configuration was started (its log file is now the
/// monitored one), so the caller can skip starting a pipeline of its own.
fn restart_running_config(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
    config_path: Option<&Path>,
) -> bool {
    let Some(name) = config_path
        .map(config::load)
        .unwrap_or_default()
        .running_config
        .filter(|name| !name.trim().is_empty())
    else {
        return false;
    };
    let config_id = {
        let a = app.lock().unwrap();
        a.configs
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&name) && !c.running)
            .map(|c| c.id)
    };
    let Some(config_id) = config_id else {
        return false;
    };
    start_config_process(window, app, charts, config_path, config_id, "Started.");
    prefill_control(window, app);
    true
}

/// Pre-create the configurations from the launcher scripts in `folder`
/// (v0.3): the parsed entries mapped to the in-memory state (with the
/// session-unique ids and the next id to hand out), or `None` when there is
/// nothing to import.
fn import_configs_from_folder(folder: &Path) -> Option<(Vec<ControlConfig>, u64)> {
    let imported = config_import::scan_folder(folder, &[]);
    if imported.is_empty() {
        return None;
    }
    let configs = imported
        .iter()
        .enumerate()
        .map(|(i, e)| ControlConfig {
            id: (i as u64) + 1,
            name: e.name.clone(),
            command_line: e.command_line.clone(),
            running: false,
            error: None,
        })
        .collect();
    Some((configs, imported.len() as u64 + 1))
}

/// Pre-create the configurations from the launcher scripts in `folder`
/// (v0.3): only when the app has no configurations yet (the user's existing
/// configurations are never touched automatically) and `folder` is an
/// existing directory. The parsed configurations are added, persisted, and
/// the Control tab is refreshed.
fn import_configs_if_empty(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    config_path: Option<&Path>,
    folder: &Path,
) {
    let imported = {
        let a = app.lock().unwrap();
        if !a.configs.is_empty() || !folder.is_dir() {
            Vec::new()
        } else {
            config_import::scan_folder(folder, &[])
        }
    };
    if imported.is_empty() {
        return;
    }
    {
        let mut a = app.lock().unwrap();
        for entry in &imported {
            let id = a.next_config_id;
            a.next_config_id += 1;
            a.configs.push(ControlConfig {
                id,
                name: entry.name.clone(),
                command_line: entry.command_line.clone(),
                running: false,
                error: None,
            });
        }
        a.control_status = import_status_message(imported.len());
        persist_configs(&a, config_path);
    }
    prefill_control(window, app);
}

/// The status line for an import of `count` configurations (v0.3).
fn import_status_message(count: usize) -> String {
    format!(
        "Imported {count} configuration{}.",
        if count == 1 { "" } else { "s" }
    )
}

/// Apply an edit to the selected configuration (v0.3 M3): the name must be
/// non-empty (after trimming) and unique (case-insensitive) among the other
/// configurations, and the command line must be non-empty (after trimming).
/// On success the configuration is updated and persisted and `"Saved."` is
/// returned; on failure the error is returned and nothing is changed.
/// Returns `None` when there is no selected configuration or the edit is a
/// no-op (the trimmed name and command line are unchanged).
fn apply_control_edit(
    app: &mut App,
    config_path: Option<&Path>,
    name: &str,
    command_line: &str,
) -> Option<String> {
    let sel = app.selected_config?;
    if sel >= app.configs.len() {
        return None;
    }
    let trimmed_name = name.trim().to_owned();
    if trimmed_name == app.configs[sel].name && command_line == app.configs[sel].command_line {
        return None;
    }
    let others: Vec<String> = app
        .configs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != sel)
        .map(|(_, c)| c.name.clone())
        .collect();
    if let Err(error) = configs::validate_name(name, &others) {
        return Some(error);
    }
    if let Err(error) = configs::validate_command_line(command_line) {
        return Some(error);
    }
    let old_name = app.configs[sel].name.clone();
    app.configs[sel].name = trimmed_name.clone();
    app.configs[sel].command_line = command_line.to_owned();
    persist_configs(app, config_path);
    if old_name != trimmed_name {
        rename_running_config_if(config_path, &old_name, &trimmed_name);
    }
    Some("Saved.".to_owned())
}

/// Commit the pending edit (the current name / command line fields) to the
/// selected configuration (v0.3 M3). Returns the status to show, if any.
fn commit_pending_edit(
    window: &MainWindow,
    app: &mut App,
    config_path: Option<&Path>,
) -> Option<String> {
    let name = window.get_control_name().as_str().to_owned();
    let command_line = window.get_control_command_line().as_str().to_owned();
    apply_control_edit(app, config_path, &name, &command_line)
}

/// Push the Control-tab state (rows, selection, enablement, status) into the
/// UI (v0.3 M3). The name / command line fields are left untouched so an
/// in-progress (or rejected) edit is not clobbered.
fn refresh_control(window: &MainWindow, app: &App) {
    let rows: Vec<ConfigRow> = app
        .configs
        .iter()
        .map(|c| ConfigRow {
            name: c.name.clone().into(),
            running: c.running,
            error: c.error.is_some(),
        })
        .collect();
    window.set_control_rows(ModelRc::from(Rc::new(slint::VecModel::<ConfigRow>::from(
        rows,
    ))));
    window.set_control_selected(match app.selected_config {
        Some(i) => i as i32,
        None => -1,
    });
    let has_sel = app.selected_config.is_some();
    let any_running = app.configs.iter().any(|c| c.running);
    // Stop acts on the selected configuration, so it is enabled only when
    // the selected configuration is running (an enabled button must always
    // have an effect).
    let sel_running = app
        .selected_config
        .and_then(|s| app.configs.get(s))
        .is_some_and(|c| c.running);
    window.set_control_start_enabled(!any_running && has_sel);
    window.set_control_restart_enabled(any_running && has_sel);
    window.set_control_stop_enabled(sel_running);
    window.set_control_delete_enabled(has_sel);
    window.set_control_status(app.control_status.clone().into());
    window.set_control_status_error(app.control_status.starts_with("Error:"));
}

/// Load the selected configuration's name and command line into the edit
/// fields (or clear them when nothing is selected) (v0.3 M3).
fn load_fields(window: &MainWindow, app: &App) {
    match app.selected_config.and_then(|i| app.configs.get(i)) {
        Some(c) => {
            window.set_control_name(c.name.clone().into());
            window.set_control_command_line(c.command_line.clone().into());
        }
        None => {
            window.set_control_name(String::new().into());
            window.set_control_command_line(String::new().into());
        }
    }
}

/// Refresh the Control-tab UI and load the selected configuration into the
/// edit fields (v0.3 M3); used when the tab is shown and after a selection,
/// add, or delete.
fn prefill_control(window: &MainWindow, app: &Arc<Mutex<App>>) {
    let a = app.lock().unwrap();
    refresh_control(window, &a);
    load_fields(window, &a);
}

/// Row-selected handler (v0.3 M3): commit the pending edit to the currently
/// selected configuration, then select the clicked row and load it into the
/// edit panel.
fn handle_control_config_selected(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    config_path: Option<&Path>,
    index: i32,
) {
    {
        let mut a = app.lock().unwrap();
        if let Some(status) = commit_pending_edit(window, &mut a, config_path) {
            a.control_status = status;
        }
        let idx = index as usize;
        a.selected_config = if idx < a.configs.len() {
            Some(idx)
        } else {
            None
        };
        if let Some(sel) = a.selected_config
            && let Some(error) = a.configs.get(sel).and_then(|c| c.error.clone())
        {
            a.control_status = error;
        }
    }
    prefill_control(window, app);
}

/// Add handler (v0.3 M3): commit the pending edit, create a configuration
/// with a unique default name and an empty command line, select it, and
/// persist it.
fn handle_control_add(window: &MainWindow, app: &Arc<Mutex<App>>, config_path: Option<&Path>) {
    {
        let mut a = app.lock().unwrap();
        let _ = commit_pending_edit(window, &mut a, config_path);
        let names: Vec<String> = a.configs.iter().map(|c| c.name.clone()).collect();
        let name = configs::next_config_name(&names);
        let id = a.next_config_id;
        a.next_config_id += 1;
        let new_index = a.configs.len();
        a.configs.push(ControlConfig {
            id,
            name,
            command_line: String::new(),
            running: false,
            error: None,
        });
        a.selected_config = Some(new_index);
        a.control_status = "Added.".to_owned();
        persist_configs(&a, config_path);
    }
    prefill_control(window, app);
    window.set_control_focus_name(window.get_control_focus_name() + 1);
}

/// Delete handler (v0.3 M3/M4): commit the pending edit, stop the selected
/// configuration's process if it is running, remove the configuration, clear
/// the selection so the edit panel shows the placeholder, and persist.
fn handle_control_delete(window: &MainWindow, app: &Arc<Mutex<App>>, config_path: Option<&Path>) {
    {
        let mut a = app.lock().unwrap();
        let _ = commit_pending_edit(window, &mut a, config_path);
        if let Some(sel) = a.selected_config.filter(|&s| s < a.configs.len()) {
            let config_id = a.configs[sel].id;
            let name = a.configs[sel].name.clone();
            if a.configs[sel].running {
                stop_config_process(&mut a, config_id);
            }
            a.configs.remove(sel);
            a.selected_config = None;
            a.control_status = "Deleted.".to_owned();
            persist_configs(&a, config_path);
            drop(a);
            clear_running_config_if(config_path, &name);
        }
    }
    prefill_control(window, app);
}

/// The process action requested from the Control tab (v0.3 M4, PRD §4.3).
enum ProcessAction {
    Start,
    Restart,
    Stop,
}

/// Kill the process tree rooted at `pid` (Windows): `taskkill /F /T`
/// terminates the process and every child it spawned, so no orphaned grandchild
/// survives to hold the pipe open (which would keep the detached reader threads
/// alive) or leak GPU memory after the direct child is killed.
#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Stop the tracked process of the configuration `config_id` (v0.3 M4):
/// kill the whole process tree, wait for the child's exit (errors are ignored
/// — the process may have already exited), and clear the running state. The
/// reader threads are left detached (never joined on the UI thread).
fn stop_config_process(app: &mut App, config_id: u64) {
    if let Some(mut tracked) = app.processes.remove(&config_id) {
        #[cfg(windows)]
        kill_process_tree(tracked.child.id());
        let _ = tracked.child.kill();
        let _ = tracked.child.wait();
    }
    if let Some(config) = app.configs.iter_mut().find(|c| c.id == config_id) {
        config.running = false;
    }
}

fn stop_all_config_processes(app: &Arc<Mutex<App>>) {
    let mut a = app.lock().unwrap();
    let ids: Vec<u64> = a.processes.keys().copied().collect();
    for id in ids {
        stop_config_process(&mut a, id);
    }
}

/// Create the app-managed log folder and delete the previous log file
/// (v0.3 M4, PRD §4.2): a missing file is benign (the first start), and any
/// other failure (e.g. the file is locked) is returned so the caller can
/// surface it in the status line without blocking the start.
fn delete_previous_log_file(log_path: &Path) -> Option<std::io::Error> {
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::remove_file(log_path) {
        Ok(()) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(e),
    }
}

/// Start the configuration `config_id`'s process (v0.3 M4, PRD §4.2/§4.3):
/// resolve the program, determine the log file (creating the app-managed log
/// folder and deleting the previous app-managed log file), spawn
/// `ninfer-serve` with the working directory set to the installation folder,
/// track the child, and point the Monitor tab at the determined log file.
/// `control_status` carries the outcome: `started_message` ("Started." for a
/// Start, "Restarted." for a Restart) or the error.
fn start_config_process(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
    config_path: Option<&Path>,
    config_id: u64,
    started_message: &'static str,
) {
    let (install_folder, log_path, running_name) = {
        let mut a = app.lock().unwrap();
        let Some(install_folder) = a.install_folder.clone() else {
            a.control_status = "Error: no installation folder configured".to_owned();
            let message = a.control_status.clone();
            if let Some(config) = a.configs.iter_mut().find(|c| c.id == config_id) {
                config.error = Some(message);
            }
            return;
        };
        let Some(config) = a.configs.iter().find(|c| c.id == config_id) else {
            return;
        };
        let command_line = config.command_line.trim();
        if command_line.is_empty() {
            a.control_status = "Error: the command line is empty".to_owned();
            let message = a.control_status.clone();
            if let Some(config) = a.configs.iter_mut().find(|c| c.id == config_id) {
                config.error = Some(message);
            }
            return;
        }
        let log_file_folder = config_path
            .map(config::load)
            .unwrap_or_default()
            .log_file_folder;
        let args = process_control::tokenize(command_line);
        let (args, log_path, app_managed) = process_control::determine_log_file(
            &args,
            &install_folder,
            Path::new(&log_file_folder),
        );
        // A non-benign deletion failure is surfaced in the status line after
        // a successful start (PRD §4.2).
        let delete_error = if app_managed {
            delete_previous_log_file(&log_path)
        } else {
            None
        };
        let program = process_control::program_path(&install_folder);
        if !program.is_file() {
            a.control_status = format!(
                "Error: failed to start: the executable does not exist: {}",
                program.display()
            );
            let message = a.control_status.clone();
            if let Some(config) = a.configs.iter_mut().find(|c| c.id == config_id) {
                config.error = Some(message);
            }
            return;
        }
        // stdout/stderr are piped and drained in the background: the child
        // never blocks on a full pipe buffer, and the captured tail is shown
        // when the start fails or the process exits abnormally (PRD §4.4).
        let mut command = std::process::Command::new(&program);
        command
            .args(&args)
            .current_dir(&install_folder)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let spawn = command.spawn();
        let running_name = match spawn {
            Ok(child) => {
                a.processes.insert(config_id, TrackedProcess::new(child));
                if let Some(config) = a.configs.iter_mut().find(|c| c.id == config_id) {
                    config.running = true;
                    config.error = None;
                }
                a.control_status = match delete_error {
                    Some(e) => format!(
                        "{started_message} (the previous log file could not be deleted: {e})"
                    ),
                    None => started_message.to_owned(),
                };
                a.configs
                    .iter()
                    .find(|c| c.id == config_id)
                    .map(|c| c.name.clone())
            }
            Err(error) => {
                a.control_status = format!("Error: failed to start: {error}");
                let message = a.control_status.clone();
                if let Some(config) = a.configs.iter_mut().find(|c| c.id == config_id) {
                    config.error = Some(message);
                }
                return;
            }
        };
        (install_folder, log_path, running_name)
    };
    if let Some(name) = running_name {
        set_running_config(config_path, &name);
    }
    switch_log_file(
        window,
        app,
        charts,
        config_path,
        Some(&install_folder),
        log_path,
    );
}

/// The user-facing form of an exit status for the Control-tab error message.
fn format_exit_status(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "an abnormal exit".to_owned(),
    }
}

/// Poll the tracked child processes on the UI tick (v0.3 M4, PRD §4.4): a
/// child that has exited (crash, external kill, or Stop) is removed and its
/// configuration's running state is cleared; when any process exited, the
/// Control tab is refreshed so the indicator and the button enablement
/// follow. An error from `try_wait` is treated as an exit so a process can
/// never get stuck as running. A failed exit marks the configuration red and
/// shows the captured console tail in the status line.
fn poll_processes(window: &MainWindow, app: &Arc<Mutex<App>>, config_path: Option<&Path>) {
    let (any_exited, exited_names) = {
        let mut a = app.lock().unwrap();
        let mut any_exited = false;
        let mut exited_names = Vec::new();
        let ids: Vec<u64> = a.processes.keys().copied().collect();
        for id in ids {
            let exit_status = match a.processes.get_mut(&id) {
                Some(tracked) => match tracked.child.try_wait() {
                    Ok(None) => None,
                    Ok(Some(status)) => Some(Ok(status)),
                    Err(_) => Some(Err(())),
                },
                None => None,
            };
            if exit_status.is_some() {
                // The reader threads are detached (never joined on the UI tick):
                // a lingering grandchild holding the pipe would otherwise block
                // a join forever and freeze the UI. The reader drains as fast as
                // possible, so the captured tail is complete in practice.
                let output = a.processes.remove(&id).map(|tracked| tracked.output_text());
                if let Some(config) = a.configs.iter_mut().find(|c| c.id == id) {
                    exited_names.push(config.name.clone());
                    config.running = false;
                    match exit_status {
                        Some(Ok(status)) if status.success() => {}
                        Some(Ok(status)) => {
                            let mut message = format!(
                                "Error: the process exited with {}",
                                format_exit_status(&status)
                            );
                            if let Some(tail) =
                                output.as_deref().map(str::trim).filter(|t| !t.is_empty())
                            {
                                message.push('\n');
                                message.push_str(tail);
                            }
                            config.error = Some(message.clone());
                            a.control_status = message;
                        }
                        Some(Err(())) => {
                            let message = "Error: the process exited".to_owned();
                            config.error = Some(message.clone());
                            a.control_status = message;
                        }
                        None => {}
                    }
                }
                any_exited = true;
            }
        }
        (any_exited, exited_names)
    };
    if any_exited {
        for name in &exited_names {
            clear_running_config_if(config_path, name);
        }
        let a = app.lock().unwrap();
        refresh_control(window, &a);
    }
}

/// Start / Restart / Stop handler (v0.3 M4, PRD §4.3): commit the pending
/// edit, then perform the requested process action on the selected
/// configuration.
fn handle_control_process_action(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
    config_path: Option<&Path>,
    action: ProcessAction,
) {
    let saved = {
        let mut a = app.lock().unwrap();
        match commit_pending_edit(window, &mut a, config_path) {
            Some(status) => {
                a.control_status = status.clone();
                status == "Saved."
            }
            None => false,
        }
    };
    let selected = {
        let a = app.lock().unwrap();
        a.selected_config.filter(|&s| s < a.configs.len()).map(|s| {
            (
                a.configs[s].id,
                a.configs[s].name.clone(),
                a.configs[s].running,
            )
        })
    };
    match action {
        ProcessAction::Stop => {
            if let Some((config_id, name, _)) = selected.filter(|&(_, _, running)| running) {
                let mut a = app.lock().unwrap();
                stop_config_process(&mut a, config_id);
                a.control_status = "Stopped.".to_owned();
                drop(a);
                clear_running_config_if(config_path, &name);
            }
        }
        ProcessAction::Start | ProcessAction::Restart => {
            let started_message = match action {
                ProcessAction::Start => "Started.",
                _ => "Restarted.",
            };
            if let Some((config_id, _, running)) = selected {
                if running {
                    // Restart: stop the old process and wait for its exit
                    // before starting the new one (PRD §4.3).
                    let mut a = app.lock().unwrap();
                    stop_config_process(&mut a, config_id);
                }
                start_config_process(window, app, charts, config_path, config_id, started_message);
            }
        }
    }
    let a = app.lock().unwrap();
    refresh_control(window, &a);
    if saved {
        load_fields(window, &a);
    }
}

/// Edit-paused handler (v0.3 M3): apply the debounced (or focus-change) edit
/// to the selected configuration and refresh the UI.
fn handle_control_edit_paused(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    config_path: Option<&Path>,
    name: slint::SharedString,
    command_line: slint::SharedString,
) {
    let saved = {
        let mut a = app.lock().unwrap();
        match apply_control_edit(&mut a, config_path, name.as_str(), command_line.as_str()) {
            Some(status) => {
                a.control_status = status.clone();
                status == "Saved."
            }
            None => false,
        }
    };
    let a = app.lock().unwrap();
    refresh_control(window, &a);
    if saved {
        load_fields(window, &a);
    }
}

/// Settings-save handler (FR-7.1/M3/M8, M1, v0.3 M2): persist the log file
/// poll interval, the GPU poll interval, the max request rows, the
/// update-check settings, the NInfer installation folder, and the log file
/// folder, and — when any of the monitored log file, the log file poll
/// interval, or the max request rows changed — restart the tail (reopen,
/// backfill, reset the view) so the new settings take effect immediately
/// instead of only for the next opened file. The monitored log file is
/// re-determined from the new log file folder
/// (`<log_file_folder>/ninfer-monitor/server.requests.jsonl`); a missing
/// file is still switched to: the tail shows Disconnected and retries until
/// the file becomes readable. An empty install folder field keeps the
/// current folder (no status-bar switch); a changed install folder updates
/// the status bar and the app state without restarting the tail. Disabling
/// the update check clears the update-available state immediately (M8 §5.1).
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
    install_folder: &str,
    log_file_folder: &str,
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
    let install_folder = install_folder.trim().to_owned();
    // A blank install folder keeps the current one (the Save button is
    // disabled for a blank/invalid folder, so this is defensive).
    let install_folder = if install_folder.is_empty() {
        app.lock()
            .unwrap()
            .install_folder
            .clone()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    } else {
        install_folder
    };
    let log_file_folder = log_file_folder.trim();
    let log_file_folder = if log_file_folder.is_empty() {
        config::default_log_file_folder()
    } else {
        log_file_folder.to_owned()
    };
    if let Some(cp) = config_path {
        let _write = config::write_lock();
        let mut cfg = config::load(cp);
        cfg.install_folder = if install_folder.is_empty() {
            None
        } else {
            Some(install_folder.clone())
        };
        cfg.log_file_folder = log_file_folder.clone();
        cfg.log_poll_interval_ms = log_poll_ms;
        cfg.gpu_poll_interval_ms = gpu_poll_ms;
        cfg.max_requests = max_requests;
        cfg.temp_unit = unit.as_str().to_owned();
        cfg.update_check_enabled = update_check_enabled;
        cfg.update_check_period_days = update_check_days;
        let _ = config::save(cp, &cfg);
    }
    // Pre-create the configurations from the launcher scripts in the
    // installation folder (v0.3): only when the app has no configurations
    // yet.
    import_configs_if_empty(window, app, config_path, Path::new(&install_folder));
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
    // Re-determine the monitored log file from the new log file folder and
    // switch the pipeline when anything that affects the tail changed.
    let new_path = startup::monitored_log_path(&log_file_folder);
    let (current, current_poll, current_max, current_folder) = {
        let app = app.lock().unwrap();
        let (poll, max) = match app.pipeline.as_ref() {
            Some(p) => (Some(p.poll_interval()), Some(p.max_requests())),
            None => (None, None),
        };
        (app.path.clone(), poll, max, app.install_folder.clone())
    };
    let new_poll = Duration::from_millis(log_poll_ms);
    let file_changed = current.as_deref() != Some(new_path.as_path());
    let poll_changed = current_poll != Some(new_poll);
    let max_changed = current_max != Some(max_requests as usize);
    let folder = if install_folder.is_empty() {
        None
    } else {
        Some(PathBuf::from(install_folder.as_str()))
    };
    if !file_changed && !poll_changed && !max_changed {
        // A changed install folder alone does not restart the tail, but the
        // status bar and the app state must follow it.
        if folder != current_folder {
            app.lock().unwrap().install_folder = folder.clone();
            if let Some(f) = &folder {
                set_install_folder(window, f);
            }
        }
        return;
    }
    switch_log_file(
        window,
        app,
        charts,
        config_path,
        folder.as_deref(),
        new_path,
    );
}

/// Settings-discard handler (M1): revert the edits made in the Settings tab
/// by re-filling the fields from the current settings; nothing is persisted.
fn handle_settings_discard(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    update: &Arc<Mutex<UpdateState>>,
    config_path: Option<&Path>,
) {
    prefill_settings(window, app, update, config_path);
}

/// About-open handler: show the About dialog over the main window.
fn handle_about_open(window: &MainWindow) {
    window.set_about_visible(true);
}

/// About-close handler: hide the About dialog, returning to the main window
/// behind it.
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

/// Log-file switch handler (FR-7.1, v0.3 M2): replace the pipeline with one
/// tailing `log_path`, reset the view, and show `install_folder` in the
/// status bar. The old pipeline is stopped on a background thread so the UI
/// thread is not blocked by the thread joins (NFR-2).
fn switch_log_file(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
    config_path: Option<&Path>,
    install_folder: Option<&Path>,
    log_path: PathBuf,
) {
    let (log_poll_interval, max_requests) = settings_for_new_file(config_path);
    let old_pipeline = {
        let mut app = app.lock().unwrap();
        let old = app.pipeline.take();
        app.pipeline = Some(Pipeline::start(
            &log_path,
            log_poll_interval,
            max_requests as usize,
        ));
        app.path = Some(log_path.clone());
        app.install_folder = install_folder.map(PathBuf::from);
        old
    };
    if let Some(old) = old_pipeline {
        std::thread::spawn(move || old.stop());
    }
    if let Some(folder) = install_folder {
        set_install_folder(window, folder);
    }
    window.set_connection_status("—".into());
    window.set_status_color(COLOR_NONE);
    let mut state = charts.lock().unwrap();
    reset_view(window, &mut state);
}

/// Start handler (PRD v0.3 §3.2.1/M2): re-check the selected folder's validity
/// at click time, persist it as `install_folder`, and switch the window to the
/// dashboard. The persisted configuration (if any) is restarted — it points
/// the Monitor tab at its log file; otherwise the pipeline is started on the
/// app-managed log file. A folder that became invalid since the enabled state
/// was last computed is ignored (the app does not wait for a folder that
/// becomes valid later). The log file does not need to exist: the tail shows
/// Disconnected and retries.
fn handle_startup_start(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    charts: &Arc<Mutex<ChartState>>,
    config_path: Option<&Path>,
) {
    let folder_text = window.get_startup_folder();
    let folder = folder_text.as_str().trim();
    let folder_path = Path::new(folder);
    if !startup::is_valid_install_folder(folder_path) {
        return;
    }
    let cfg = config_path.map(config::load).unwrap_or_default();
    let log_path = startup::monitored_log_path(&cfg.log_file_folder);
    persist_install_folder(config_path, folder_path);
    // Pre-create the configurations from the launcher scripts in the folder
    // (v0.3): only when the app has no configurations yet.
    import_configs_if_empty(window, app, config_path, folder_path);
    window.set_startup_visible(false);
    // Record the installation folder so a restarted configuration can resolve
    // its program and log file.
    app.lock().unwrap().install_folder = Some(folder_path.to_path_buf());
    // Restart the persisted configuration (if any): it points the Monitor tab
    // at the configuration's log file (starting the pipeline) via
    // `switch_log_file`. Only when there is no such configuration do we start
    // a pipeline of our own on the app-managed log file — avoiding a redundant
    // pipeline that would be replaced immediately.
    let restarted = restart_running_config(window, app, charts, config_path);
    if !restarted {
        switch_log_file(
            window,
            app,
            charts,
            config_path,
            Some(folder_path),
            log_path,
        );
    }
}

/// Close handler (PRD v0.3 §3.2.1/M2): persist the window geometry (FR-7.4) and
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

/// Folder-edited handler (PRD v0.3 §3.2.1/M2): update the Start button's
/// enabled state to reflect whether the edited folder is a valid NInfer
/// installation folder. The state is only recomputed on edits (no retry or
/// auto-enable).
fn handle_startup_folder_edited(window: &MainWindow, text: &str) {
    window.set_startup_start_enabled(startup::is_valid_install_folder(Path::new(text)));
}

/// Settings folder-edited handler (v0.3 M2): update the Save button's
/// enabled state to reflect whether the install folder is a valid NInfer
/// installation folder and the log file folder is an existing directory,
/// and the "Import Configurations" button's enabled state to reflect whether
/// the install folder is an existing directory (v0.3). The state is only
/// recomputed on edits (no retry or auto-enable).
fn handle_settings_folder_edited(window: &MainWindow, _text: &str) {
    let install_folder = window.get_settings_install_folder().as_str().to_owned();
    let log_file_folder = window.get_settings_log_file_folder().as_str().to_owned();
    window.set_settings_save_enabled(settings_save_enabled(&install_folder, &log_file_folder));
    window.set_settings_import_enabled(settings_import_enabled(&install_folder));
}

/// Settings-tab "Import Configurations" handler (v0.3): scan the currently
/// selected installation folder for launcher scripts and add the parsed
/// configurations, skipping the names that already exist (case-insensitive,
/// dedup). The outcome is shown in the import status line; the Control tab
/// is refreshed when something was added.
fn handle_settings_import_configs(
    window: &MainWindow,
    app: &Arc<Mutex<App>>,
    config_path: Option<&Path>,
) {
    let folder = window
        .get_settings_install_folder()
        .as_str()
        .trim()
        .to_owned();
    if folder.is_empty() || !Path::new(&folder).is_dir() {
        window.set_settings_import_status(
            "Error: the installation folder is not a valid directory".into(),
        );
        return;
    }
    let added = {
        let mut a = app.lock().unwrap();
        let existing: Vec<String> = a.configs.iter().map(|c| c.name.clone()).collect();
        let imported = config_import::scan_folder(Path::new(&folder), &existing);
        let added = imported.len();
        for entry in imported {
            let id = a.next_config_id;
            a.next_config_id += 1;
            a.configs.push(ControlConfig {
                id,
                name: entry.name,
                command_line: entry.command_line,
                running: false,
                error: None,
            });
        }
        if added > 0 {
            persist_configs(&a, config_path);
        }
        added
    };
    let status = if added == 0 {
        "No new configurations found.".to_owned()
    } else {
        import_status_message(added)
    };
    window.set_settings_import_status(status.into());
    if added > 0 {
        prefill_control(window, app);
    }
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

/// Manual update check (M8, the Settings tab's "Check for updates" button):
/// run the check immediately, bypassing the due condition (the period
/// schedule and the failed-attempt throttle — the user explicitly asked for
/// it) and the enabled flag. A successful check records `last_check_unix_ms`
/// (persisted, so the automatic schedule restarts from it) and mirrors the
/// found release into `available` (the user asked for the result, so it is
/// mirrored even when the automatic check is disabled); a failed check keeps
/// the previous state. Returns the check's outcome so the caller can decide
/// whether to show the update dialog.
fn run_manual_update_check_with<F>(
    update: &Arc<Mutex<UpdateState>>,
    config_path: Option<&Path>,
    now_unix_ms: u64,
    check: F,
) -> Result<Option<LatestRelease>, update_check::CheckError>
where
    F: FnOnce() -> Result<Option<LatestRelease>, update_check::CheckError>,
{
    let outcome = check();
    let success = outcome.is_ok();
    {
        let mut s = update.lock().unwrap();
        s.last_attempt_unix_ms = Some(now_unix_ms);
        if let Ok(release) = outcome.as_ref() {
            s.last_check_unix_ms = Some(now_unix_ms);
            s.available = release.clone();
        }
    }
    if success {
        persist_last_update_check(config_path, now_unix_ms);
    }
    outcome
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

/// Mirror the current update-available state into the UI (M8): the tab-bar
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

/// Mirror a successful manual check's outcome into the UI (M8): the update
/// dialog when a release is found, the latest-version dialog when not.
fn apply_manual_check_result(window: &MainWindow, available: Option<&LatestRelease>) {
    apply_update_available(window, available);
    match available {
        Some(_) => window.set_update_visible(true),
        None => {
            window.set_latest_version(env!("CARGO_PKG_VERSION").into());
            window.set_latest_version_visible(true);
        }
    }
}

/// Settings-tab "Check for updates" handler (M8): run the network check
/// immediately in a background thread (the event loop stays responsive,
/// NFR-2) and mirror the outcome into the UI on the event loop — the update
/// dialog when a release is found, the latest-version dialog when not. A
/// failed check is silent (M8 §5.1). `in_flight` guards against re-clicks
/// while a check is running.
fn handle_settings_check_updates(
    window: &MainWindow,
    update: &Arc<Mutex<UpdateState>>,
    in_flight: &Arc<AtomicBool>,
    config_path: Option<PathBuf>,
) {
    // Ignore re-clicks while a check is already in flight.
    if in_flight
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let weak = window.as_weak();
    let update = update.clone();
    let in_flight = in_flight.clone();
    std::thread::spawn(move || {
        let outcome = run_manual_update_check_with(
            &update,
            config_path.as_deref(),
            update_check::now_unix_ms(),
            || update_check::check(env!("CARGO_PKG_VERSION")),
        );
        let guard = in_flight.clone();
        let invoked = slint::invoke_from_event_loop(move || {
            guard.store(false, Ordering::SeqCst);
            let Some(window) = weak.upgrade() else {
                return;
            };
            if let Ok(available) = outcome {
                apply_manual_check_result(&window, available.as_ref());
            }
        });
        if invoked.is_err() {
            // The event loop is gone; reset the flag so it cannot stick.
            in_flight.store(false, Ordering::SeqCst);
        }
    });
}

/// Update-dialog-close handler (M8): hide the dialog. The update button
/// stays visible so the dialog can be reopened later. A download in flight
/// is canceled by the caller (the close callback sets the cancel flag), so
/// this also serves as the download cancel.
fn handle_update_close(window: &MainWindow) {
    window.set_update_visible(false);
}

/// Latest-version-dialog-close handler (M8): hide the dialog.
fn handle_latest_version_close(window: &MainWindow) {
    window.set_latest_version_visible(false);
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
    // The registered NInfer configurations (v0.3 M3), loaded once at startup.
    let (control_configs, next_config_id) = load_control_configs(&cfg.configs);
    let resolved = resolve(&cfg);
    // The CLI install folder takes precedence over the configured one
    // (v0.3 M2); it is not persisted unless the user presses Start or Save.
    let install_folder = cli
        .install_folder
        .clone()
        .or(resolved.install_folder.clone());
    let startup = startup::startup_state(install_folder.as_deref());
    let log_path = startup::monitored_log_path(&resolved.log_file_folder);
    // Pre-create the configurations from the launcher scripts in the
    // installation folder (v0.3): only when the app has no configurations
    // yet and a valid folder is resolved (the startup screen is skipped).
    let (control_configs, next_config_id) = if control_configs.is_empty()
        && !startup.visible
        && let Some(folder) = install_folder.as_deref()
    {
        if let Some((configs, next_id)) = import_configs_from_folder(folder) {
            if let Some(cp) = &config_path {
                let _write = config::write_lock();
                let mut saved = config::load(cp);
                saved.configs = configs
                    .iter()
                    .map(|c| ConfigEntry {
                        name: c.name.clone(),
                        command_line: c.command_line.clone(),
                    })
                    .collect();
                let _ = config::save(cp, &saved);
            }
            (configs, next_id)
        } else {
            (control_configs, next_config_id)
        }
    } else {
        (control_configs, next_config_id)
    };

    let window = MainWindow::new()?;
    window
        .global::<Palette>()
        .set_color_scheme(ColorScheme::Dark);
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
    // Restore the Control-tab split fraction (the configurations list share).
    window.set_control_split_frac(cfg.control_split);
    let split = Arc::new(Mutex::new(cfg.split_panels));
    let control_split = Arc::new(Mutex::new(cfg.control_split));
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
            Some(Pipeline::start(
                &log_path,
                resolved.log_poll_interval,
                resolved.max_requests as usize,
            ))
        },
        path: if startup.visible {
            None
        } else {
            Some(log_path.clone())
        },
        install_folder: if startup.visible {
            None
        } else {
            install_folder.clone()
        },
        configs: control_configs,
        selected_config: None,
        next_config_id,
        control_status: String::new(),
        processes: std::collections::HashMap::new(),
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
        window.set_startup_folder(startup.folder.into());
        window.set_startup_start_enabled(false);
    } else {
        apply_startup_state(&window, install_folder.as_deref());
        restart_running_config(&window, &app, &charts, config_path.as_deref());
    }
    let tracked_state = Arc::new(Mutex::new(None::<WindowState>));
    // Update-install flow (M8 §5.3): `install-in-flight` guards against
    // re-clicks, `install-cancel` is set by the dialog close while a
    // download runs, and `pending-install` hands the downloaded installer to
    // the post-event-loop launch below.
    let install_in_flight = Arc::new(AtomicBool::new(false));
    let install_cancel = Arc::new(AtomicBool::new(false));
    let pending_install: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    // Manual update-check flow (M8): `check-in-flight` guards against
    // re-clicks while the background check runs.
    let check_in_flight = Arc::new(AtomicBool::new(false));

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
    window.on_control_split_changed({
        let weak = window.as_weak();
        let control_split = control_split.clone();
        move |frac: f32| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_control_split_changed(&ui_window, &control_split, frac);
        }
    });
    window.on_control_split_released({
        let config_path = config_path.clone();
        let control_split = control_split.clone();
        move |_frac: f32| {
            handle_control_split_released(config_path.as_deref(), &control_split);
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
    window.on_tab_changed({
        let weak = window.as_weak();
        let app = app.clone();
        let update = update.clone();
        let config_path = config_path.clone();
        move |which: i32| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_tab_changed(&ui_window, &app, &update, config_path.as_deref(), which);
        }
    });
    window.on_control_config_selected({
        let weak = window.as_weak();
        let app = app.clone();
        let config_path = config_path.clone();
        move |index: i32| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_control_config_selected(&ui_window, &app, config_path.as_deref(), index);
        }
    });
    window.on_control_add({
        let weak = window.as_weak();
        let app = app.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_control_add(&ui_window, &app, config_path.as_deref());
        }
    });
    window.on_control_delete({
        let weak = window.as_weak();
        let app = app.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_control_delete(&ui_window, &app, config_path.as_deref());
        }
    });
    window.on_control_start({
        let weak = window.as_weak();
        let app = app.clone();
        let charts = charts.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_control_process_action(
                &ui_window,
                &app,
                &charts,
                config_path.as_deref(),
                ProcessAction::Start,
            );
        }
    });
    window.on_control_restart({
        let weak = window.as_weak();
        let app = app.clone();
        let charts = charts.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_control_process_action(
                &ui_window,
                &app,
                &charts,
                config_path.as_deref(),
                ProcessAction::Restart,
            );
        }
    });
    window.on_control_stop({
        let weak = window.as_weak();
        let app = app.clone();
        let charts = charts.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_control_process_action(
                &ui_window,
                &app,
                &charts,
                config_path.as_deref(),
                ProcessAction::Stop,
            );
        }
    });
    window.on_control_edit_paused({
        let weak = window.as_weak();
        let app = app.clone();
        let config_path = config_path.clone();
        move |name: slint::SharedString, command_line: slint::SharedString| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_control_edit_paused(
                &ui_window,
                &app,
                config_path.as_deref(),
                name,
                command_line,
            );
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
              install_folder: slint::SharedString,
              log_file_folder: slint::SharedString,
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
                install_folder.as_str(),
                log_file_folder.as_str(),
                temp_unit.as_str(),
                update_check_enabled,
                update_check_days,
                &update,
            );
        }
    });
    window.on_settings_install_folder_edited({
        let weak = window.as_weak();
        move |text: slint::SharedString| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_settings_folder_edited(&ui_window, text.as_str());
        }
    });
    window.on_settings_log_file_folder_edited({
        let weak = window.as_weak();
        move |text: slint::SharedString| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_settings_folder_edited(&ui_window, text.as_str());
        }
    });
    window.on_settings_check_updates({
        let weak = window.as_weak();
        let update = update.clone();
        let check_in_flight = check_in_flight.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_settings_check_updates(
                &ui_window,
                &update,
                &check_in_flight,
                config_path.clone(),
            );
        }
    });
    window.on_settings_import_configs({
        let weak = window.as_weak();
        let app = app.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_settings_import_configs(&ui_window, &app, config_path.as_deref());
        }
    });
    window.on_settings_open_folder({
        let weak = window.as_weak();
        let dialog_open = Arc::new(AtomicBool::new(false));
        move || {
            // Ignore re-clicks while a folder dialog is already open.
            if dialog_open
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            let weak = weak.clone();
            let dialog_open = dialog_open.clone();
            std::thread::spawn(move || {
                let path = pick_folder();
                let guard = dialog_open.clone();
                let invoked = slint::invoke_from_event_loop(move || {
                    guard.store(false, Ordering::SeqCst);
                    let Some(w) = weak.upgrade() else {
                        return;
                    };
                    let Some(path) = path else {
                        return;
                    };
                    w.set_settings_install_folder(path.display().to_string().into());
                    handle_settings_folder_edited(&w, "");
                });
                if invoked.is_err() {
                    dialog_open.store(false, Ordering::SeqCst);
                }
            });
        }
    });
    window.on_settings_open_log_folder({
        let weak = window.as_weak();
        let dialog_open = Arc::new(AtomicBool::new(false));
        move || {
            // Ignore re-clicks while a folder dialog is already open.
            if dialog_open
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            let weak = weak.clone();
            let dialog_open = dialog_open.clone();
            std::thread::spawn(move || {
                let path = pick_folder();
                let guard = dialog_open.clone();
                let invoked = slint::invoke_from_event_loop(move || {
                    guard.store(false, Ordering::SeqCst);
                    let Some(w) = weak.upgrade() else {
                        return;
                    };
                    let Some(path) = path else {
                        return;
                    };
                    w.set_settings_log_file_folder(path.display().to_string().into());
                    handle_settings_folder_edited(&w, "");
                });
                if invoked.is_err() {
                    dialog_open.store(false, Ordering::SeqCst);
                }
            });
        }
    });
    window.on_settings_discard({
        let weak = window.as_weak();
        let app = app.clone();
        let update = update.clone();
        let config_path = config_path.clone();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_settings_discard(&ui_window, &app, &update, config_path.as_deref());
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
    window.on_latest_version_close({
        let weak = window.as_weak();
        move || {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_latest_version_close(&ui_window);
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
    window.on_startup_open_folder({
        let weak = window.as_weak();
        let dialog_open = Arc::new(AtomicBool::new(false));
        move || {
            // Ignore re-clicks while a folder dialog is already open.
            if dialog_open
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            let weak = weak.clone();
            let dialog_open = dialog_open.clone();
            std::thread::spawn(move || {
                let path = pick_folder();
                let guard = dialog_open.clone();
                let invoked = slint::invoke_from_event_loop(move || {
                    guard.store(false, Ordering::SeqCst);
                    let Some(w) = weak.upgrade() else {
                        return;
                    };
                    let Some(path) = path else {
                        return;
                    };
                    w.set_startup_folder(path.display().to_string().into());
                    w.set_startup_start_enabled(startup::is_valid_install_folder(&path));
                });
                if invoked.is_err() {
                    dialog_open.store(false, Ordering::SeqCst);
                }
            });
        }
    });
    window.on_startup_folder_edited({
        let weak = window.as_weak();
        move |text: slint::SharedString| {
            let Some(ui_window) = weak.upgrade() else {
                return;
            };
            handle_startup_folder_edited(&ui_window, text.as_str());
        }
    });
    window.on_tick({
        let weak = window.as_weak();
        let charts = charts.clone();
        let app = app.clone();
        let gpu = gpu.clone();
        let table_columns = table_columns.clone();
        let tracked_state = tracked_state.clone();
        let config_path = config_path.clone();
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
            refresh_install_folder(&ui_window, &app);
            poll_processes(&ui_window, &app, config_path.as_deref());
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

    stop_all_config_processes(&app);
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
        match launch_installer(&installer) {
            Ok(()) => std::process::exit(0),
            Err(e) => report_installer_launch_failure(&e),
        }
    }
    Ok(())
}

fn launch_installer(installer: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::{SHELLEXECUTEINFOW, ShellExecuteExW};
        use windows::core::{PCWSTR, w};

        let verb = w!("runas");
        let file: Vec<u16> = installer
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            nShow: 1,
            ..Default::default()
        };
        info.lpVerb = PCWSTR(verb.as_ptr());
        info.lpFile = PCWSTR(file.as_ptr());
        // TODO: a cancelled UAC prompt (the `runas` verb) fails with
        // ERROR_CANCELLED and is only reported via
        // `report_installer_launch_failure` (DebugView on Windows), so the
        // user sees the app exit with no update and no explanation; surface
        // a visible message (e.g. a message box) for launch failures.
        // SAFETY: `info` is fully initialized and its `lpVerb`/`lpFile` fields
        // point at buffers that stay alive across the call.
        unsafe { ShellExecuteExW(&mut info) }.map_err(|e| std::io::Error::other(e.to_string()))
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(installer).spawn().map(|_| ())
    }
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
        install_folder: Option<&str>,
        log_poll_ms: u64,
        window_ms: u64,
        max_requests: u64,
    ) -> Config {
        Config {
            install_folder: install_folder.map(str::to_owned),
            log_file_folder: config::default_log_file_folder(),
            log_poll_interval_ms: log_poll_ms,
            gpu_poll_interval_ms: config::DEFAULT_GPU_POLL_MS,
            chart_window_ms: window_ms,
            max_requests,
            temp_unit: "F".to_owned(),
            split_panels: SplitPanels::default(),
            control_split: config::DEFAULT_CONTROL_SPLIT,
            table_columns: None,
            window_state: None,
            update_check_enabled: true,
            update_check_period_days: update_check::DEFAULT_PERIOD_DAYS,
            last_update_check_unix_ms: None,
            configs: Vec::new(),
            running_config: None,
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
    fn positional_argument_is_the_install_folder() {
        let cli = parse_cli_args(&args(&["C:/ninfer"]));
        assert_eq!(cli.config_path, None);
        assert_eq!(cli.install_folder, Some(PathBuf::from("C:/ninfer")));
    }

    #[test]
    fn no_args_yields_defaults() {
        let cli = parse_cli_args(&args(&[]));
        assert_eq!(cli.config_path, None);
        assert_eq!(cli.install_folder, None);
    }

    #[test]
    fn unknown_flags_are_skipped() {
        let cli = parse_cli_args(&args(&["--verbose", "C:/ninfer"]));
        assert_eq!(cli.config_path, None);
        assert_eq!(cli.install_folder, Some(PathBuf::from("C:/ninfer")));
    }

    #[test]
    fn config_flag_value_is_consumed_as_config_path() {
        let cli = parse_cli_args(&args(&["--config", "cfg.json", "C:/ninfer"]));
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg.json")));
        assert_eq!(cli.install_folder, Some(PathBuf::from("C:/ninfer")));
    }

    #[test]
    fn last_positional_argument_wins() {
        let cli = parse_cli_args(&args(&["C:/a", "C:/b"]));
        assert_eq!(cli.install_folder, Some(PathBuf::from("C:/b")));
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
        let cli = parse_cli_args(&args(&["--config=cfg.json", "C:/ninfer"]));
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg.json")));
        assert_eq!(cli.install_folder, Some(PathBuf::from("C:/ninfer")));
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
    fn startup_config_directory_resolves_to_config_json() {
        let dir = tempfile::tempdir().unwrap();
        let (path, cfg) = load_startup_config(Some(dir.path()));
        let expected = dir.path().join(config::CONFIG_FILE);
        assert_eq!(path, Some(expected.clone()));
        assert_eq!(cfg, Config::default());
        assert!(
            expected.exists(),
            "config.json is created inside the directory"
        );
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
    fn startup_config_clears_a_stale_running_config() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"running_config":"gone","configs":[{"name":"a","command_line":"x"}]}"#,
        )
        .unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(cfg.running_config, None);
        assert_eq!(config::load(&cp).running_config, None);
    }

    #[test]
    fn startup_config_keeps_a_matching_running_config_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"running_config":"A","configs":[{"name":"a","command_line":"x"}],"sentinel":1}"#,
        )
        .unwrap();
        let (_, cfg) = load_startup_config(Some(&cp));
        assert_eq!(cfg.running_config, Some("A".to_owned()));
        let raw = std::fs::read_to_string(&cp).unwrap();
        assert!(
            raw.contains("sentinel"),
            "a matching running_config must not be rewritten: {raw}"
        );
    }

    #[test]
    fn set_and_clear_running_config_keep_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":200,"configs":[{"name":"a","command_line":"x"}]}"#,
        )
        .unwrap();
        set_running_config(Some(&cp), "a");
        let cfg = config::load(&cp);
        assert_eq!(cfg.running_config, Some("a".to_owned()));
        assert_eq!(cfg.log_poll_interval_ms, 200);
        assert_eq!(cfg.configs.len(), 1);
        clear_running_config_if(Some(&cp), "A");
        let cfg = config::load(&cp);
        assert_eq!(cfg.running_config, None);
        assert_eq!(cfg.log_poll_interval_ms, 200);
        assert_eq!(cfg.configs.len(), 1);
        clear_running_config_if(Some(&cp), "b");
        assert_eq!(config::load(&cp).running_config, None);
    }

    #[test]
    fn settings_come_from_config() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"log_poll_interval_ms":200,"max_requests":250}"#).unwrap();
        let (poll, max_requests) = settings_for_new_file(Some(&cp));
        assert_eq!(poll, Duration::from_millis(200));
        assert_eq!(max_requests, 250);
    }

    #[test]
    fn settings_defaults_without_config() {
        let (poll, max_requests) = settings_for_new_file(None);
        assert_eq!(poll, Config::default().log_poll_interval());
        assert_eq!(max_requests, config::DEFAULT_MAX_REQUESTS);
    }

    #[test]
    fn persist_install_folder_keeps_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":200,"chart_window_ms":60000}"#,
        )
        .unwrap();
        persist_install_folder(Some(&cp), Path::new("C:/ninfer"));
        let cfg = config::load(&cp);
        assert_eq!(cfg.install_folder, Some("C:/ninfer".to_owned()));
        assert_eq!(cfg.log_poll_interval_ms, 200);
        assert_eq!(cfg.chart_window_ms, 60_000);
    }

    #[test]
    fn persist_install_folder_without_config_is_a_noop() {
        persist_install_folder(None, Path::new("C:/ninfer"));
    }

    #[test]
    fn startup_state_sets_install_folder() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        apply_startup_state(&window, None);
        assert_eq!(window.get_install_folder(), "");

        apply_startup_state(&window, Some(Path::new("C:/ninfer")));
        assert_eq!(window.get_install_folder_full(), "C:/ninfer");
        assert_eq!(window.get_install_folder(), "C:/ninfer");
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
    fn manual_check_result_shows_update_dialog_when_release_found() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        apply_manual_check_result(&window, Some(&test_release()));
        assert!(window.get_update_available());
        assert!(window.get_update_visible());
        assert!(!window.get_latest_version_visible());
    }

    #[test]
    fn manual_check_result_shows_latest_version_dialog_when_no_release() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        apply_manual_check_result(&window, None);
        assert!(!window.get_update_available());
        assert!(!window.get_update_visible());
        assert!(window.get_latest_version_visible());
        assert_eq!(window.get_latest_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn latest_version_close_hides_dialog() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window.set_latest_version_visible(true);
        handle_latest_version_close(&window);
        assert!(!window.get_latest_version_visible());
    }

    #[test]
    fn settings_discard_refills_from_config() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":200,"gpu_poll_interval_ms":3000,"max_requests":250}"#,
        )
        .unwrap();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
            install_folder: Some(PathBuf::from("C:/ninfer")),
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
        }));
        // Simulate edits made in the Settings tab.
        window.set_settings_log_poll_ms(500);
        window.set_settings_gpu_poll_ms(5_000);
        window.set_settings_max_requests(1000);
        window.set_settings_install_folder("C:/edited".into());
        handle_settings_discard(&window, &app, &test_update(), Some(&cp));
        assert_eq!(window.get_settings_log_poll_ms(), 200);
        assert_eq!(window.get_settings_gpu_poll_ms(), 3_000);
        assert_eq!(window.get_settings_max_requests(), 250);
        assert_eq!(window.get_settings_install_folder(), "C:/ninfer");
    }

    #[test]
    fn settings_save_applies_temp_unit() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
            "",
            "C",
            true,
            3,
            &test_update(),
        );
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
    fn resolve_install_folder_comes_from_config() {
        let cfg = make_config(Some("C:/ninfer"), 200, 60_000, 1_000);
        let r = resolve(&cfg);
        assert_eq!(r.install_folder, Some(PathBuf::from("C:/ninfer")));
    }

    #[test]
    fn resolve_no_install_folder_yields_none() {
        let r = resolve(&Config::default());
        assert_eq!(r.install_folder, None);
    }

    #[test]
    fn resolve_log_file_folder_comes_from_config() {
        let mut cfg = make_config(None, 200, 60_000, 1_000);
        cfg.log_file_folder = "C:/logs".to_owned();
        let r = resolve(&cfg);
        assert_eq!(r.log_file_folder, "C:/logs");
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
    fn switch_log_file_replaces_pipeline_and_sets_install_folder() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.jsonl");
        let path_b = dir.path().join("b.jsonl");
        let folder_a = dir.path().join("ninfer-a");
        let folder_b = dir.path().join("ninfer-b");
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
            install_folder: Some(folder_a.clone()),
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
        assert_eq!(
            window.get_install_folder_full(),
            folder_a.display().to_string()
        );

        switch_log_file(
            &window,
            &app,
            &charts,
            Some(&cp),
            Some(&folder_b),
            path_b.clone(),
        );
        assert_eq!(
            window.get_install_folder_full(),
            folder_b.display().to_string()
        );
        assert_eq!(window.get_kpi_decode_value(), "—", "the view is reset");
        assert_eq!(app.lock().unwrap().install_folder, Some(folder_b.clone()));

        append_throughput_line(&path_b, 2_000, 9.9);
        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_kpi_decode_value() != "—"
        });
        assert_eq!(window.get_kpi_decode_value(), "9.9");
    }

    #[test]
    fn switch_log_file_without_config_replaces_pipeline() {
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        switch_log_file(&window, &app, &charts, None, None, path_b.clone());
        assert_eq!(window.get_kpi_decode_value(), "—", "the view is reset");
    }

    #[test]
    fn settings_save_switches_pipeline_and_backfills_new_file() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let folder_a = dir.path().join("logs-a");
        let folder_b = dir.path().join("logs-b");
        let install = dir.path().join("ninfer");
        let cp = dir.path().join("config.json");
        std::fs::create_dir_all(&folder_a).unwrap();
        std::fs::create_dir_all(&folder_b).unwrap();
        let path_a = startup::monitored_log_path(&folder_a.display().to_string());
        let path_b = startup::monitored_log_path(&folder_b.display().to_string());
        std::fs::create_dir_all(path_a.parent().unwrap()).unwrap();
        std::fs::File::create(&path_a).unwrap();
        std::fs::create_dir_all(path_b.parent().unwrap()).unwrap();
        std::fs::File::create(&path_b).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(50),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path_a.clone()),
            install_folder: Some(install.clone()),
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
            &install.display().to_string(),
            &folder_b.display().to_string(),
            "F",
            true,
            3,
            &test_update(),
        );
        assert_eq!(app.lock().unwrap().path, Some(path_b.clone()));
        assert_eq!(window.get_kpi_decode_value(), "—", "the view is reset");
        assert_eq!(
            config::load(&cp).install_folder,
            Some(install.display().to_string())
        );
        assert_eq!(
            config::load(&cp).log_file_folder,
            folder_b.display().to_string()
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
        let folder = dir.path().join("logs");
        let install = dir.path().join("ninfer");
        let cp = dir.path().join("config.json");
        std::fs::create_dir_all(&folder).unwrap();
        let path_a = startup::monitored_log_path(&folder.display().to_string());
        std::fs::create_dir_all(path_a.parent().unwrap()).unwrap();
        std::fs::File::create(&path_a).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(200),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path_a.clone()),
            install_folder: Some(install.clone()),
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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

        // Saving with the same log file folder, log file poll interval, and
        // max request rows must not restart the pipeline or reset the view.
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            200,
            5_000,
            config::DEFAULT_MAX_REQUESTS as i32,
            &install.display().to_string(),
            &folder.display().to_string(),
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
    }

    #[test]
    fn settings_save_changed_install_folder_updates_status_bar_without_restart() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("logs");
        let install_a = dir.path().join("ninfer-a");
        let install_b = dir.path().join("ninfer-b");
        let cp = dir.path().join("config.json");
        std::fs::create_dir_all(&folder).unwrap();
        let path_a = startup::monitored_log_path(&folder.display().to_string());
        std::fs::create_dir_all(path_a.parent().unwrap()).unwrap();
        std::fs::File::create(&path_a).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(200),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path_a.clone()),
            install_folder: Some(install_a.clone()),
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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

        // Saving with a changed install folder (same log file folder, log
        // file poll interval, and max request rows) must update the status
        // bar and the app state without restarting the pipeline or resetting
        // the view.
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            200,
            5_000,
            config::DEFAULT_MAX_REQUESTS as i32,
            &install_b.display().to_string(),
            &folder.display().to_string(),
            "F",
            true,
            3,
            &test_update(),
        );
        assert_eq!(app.lock().unwrap().path, Some(path_a.clone()));
        assert_eq!(
            app.lock().unwrap().install_folder,
            Some(install_b.clone()),
            "the app state follows the new install folder"
        );
        assert_eq!(
            window.get_install_folder_full(),
            install_b.display().to_string(),
            "the status bar shows the new install folder"
        );
        assert_eq!(
            window.get_kpi_decode_value(),
            "1.5",
            "the view is not reset when only the install folder changed"
        );
        assert_eq!(
            config::load(&cp).install_folder,
            Some(install_b.display().to_string()),
            "the new install folder is persisted"
        );
    }

    #[test]
    fn settings_save_restarts_pipeline_when_poll_changes() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("logs");
        let install = dir.path().join("ninfer");
        let cp = dir.path().join("config.json");
        std::fs::create_dir_all(&folder).unwrap();
        let path_a = startup::monitored_log_path(&folder.display().to_string());
        std::fs::create_dir_all(path_a.parent().unwrap()).unwrap();
        std::fs::File::create(&path_a).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(200),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path_a.clone()),
            install_folder: Some(install.clone()),
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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

        // Changing only the log file poll interval (same log file folder)
        // must restart the tail so the new interval takes effect
        // immediately.
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            500,
            5_000,
            config::DEFAULT_MAX_REQUESTS as i32,
            &install.display().to_string(),
            &folder.display().to_string(),
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
        let folder = dir.path().join("logs");
        let install = dir.path().join("ninfer");
        let cp = dir.path().join("config.json");
        std::fs::create_dir_all(&folder).unwrap();
        let path_a = startup::monitored_log_path(&folder.display().to_string());
        std::fs::create_dir_all(path_a.parent().unwrap()).unwrap();
        std::fs::File::create(&path_a).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(200),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path_a.clone()),
            install_folder: Some(install.clone()),
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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

        // Changing only the max request rows (same log file folder, same
        // poll) must restart the tail so the new cap takes effect
        // immediately.
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            200,
            5_000,
            250,
            &install.display().to_string(),
            &folder.display().to_string(),
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
    fn settings_save_ignores_blank_install_folder() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("logs");
        let install = dir.path().join("ninfer");
        let cp = dir.path().join("config.json");
        std::fs::create_dir_all(&folder).unwrap();
        let path_a = startup::monitored_log_path(&folder.display().to_string());
        std::fs::create_dir_all(path_a.parent().unwrap()).unwrap();
        std::fs::File::create(&path_a).unwrap();

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(500),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path_a.clone()),
            install_folder: Some(install.clone()),
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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

        // A blank install folder field keeps the current folder (no switch).
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            500,
            5_000,
            config::DEFAULT_MAX_REQUESTS as i32,
            "   ",
            &folder.display().to_string(),
            "F",
            true,
            3,
            &test_update(),
        );
        assert_eq!(app.lock().unwrap().path, Some(path_a.clone()));
        assert_eq!(
            app.lock().unwrap().install_folder,
            Some(install.clone()),
            "the current install folder is kept for a blank field"
        );
        assert_eq!(
            window.get_kpi_decode_value(),
            "1.5",
            "the view is not reset for a blank install folder"
        );
    }

    #[test]
    fn settings_save_enabled_requires_a_valid_install_folder_and_log_dir() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("ninfer");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::File::create(install.join(startup::ninfer_serve_exe_name())).unwrap();
        let log_dir = dir.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        assert!(
            settings_save_enabled(
                &install.display().to_string(),
                &log_dir.display().to_string()
            ),
            "a valid install folder and an existing log dir enable Save"
        );
        // An install folder without the executable disables Save.
        let no_exe = dir.path().join("no-exe");
        std::fs::create_dir_all(&no_exe).unwrap();
        assert!(!settings_save_enabled(
            &no_exe.display().to_string(),
            &log_dir.display().to_string()
        ));
        // A missing log file folder disables Save.
        assert!(!settings_save_enabled(
            &install.display().to_string(),
            &dir.path().join("missing").display().to_string()
        ));
        // A blank install folder disables Save.
        assert!(!settings_save_enabled("", &log_dir.display().to_string()));
    }

    #[test]
    fn settings_folder_edited_updates_save_enabled() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("ninfer");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::File::create(install.join(startup::ninfer_serve_exe_name())).unwrap();
        let log_dir = dir.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let window = MainWindow::new().unwrap();
        window.set_settings_install_folder(install.display().to_string().into());
        window.set_settings_log_file_folder(log_dir.display().to_string().into());
        handle_settings_folder_edited(&window, "");
        assert!(
            window.get_settings_save_enabled(),
            "a valid pair enables Save"
        );

        window.set_settings_install_folder("missing".into());
        handle_settings_folder_edited(&window, "");
        assert!(
            !window.get_settings_save_enabled(),
            "an invalid install folder disables Save"
        );

        window.set_settings_install_folder(install.display().to_string().into());
        window.set_settings_log_file_folder("missing".into());
        handle_settings_folder_edited(&window, "");
        assert!(
            !window.get_settings_save_enabled(),
            "a missing log file folder disables Save"
        );

        window.set_settings_log_file_folder(log_dir.display().to_string().into());
        handle_settings_folder_edited(&window, "");
        assert!(
            window.get_settings_save_enabled(),
            "restoring a valid pair re-enables Save"
        );
    }

    #[test]
    fn settings_save_switches_to_missing_file_and_recovers() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let folder_a = dir.path().join("logs-a");
        let folder_b = dir.path().join("logs-b");
        let install = dir.path().join("ninfer");
        let cp = dir.path().join("config.json");
        std::fs::create_dir_all(&folder_a).unwrap();
        let path_a = startup::monitored_log_path(&folder_a.display().to_string());
        std::fs::create_dir_all(path_a.parent().unwrap()).unwrap();
        std::fs::File::create(&path_a).unwrap();
        let missing = startup::monitored_log_path(&folder_b.display().to_string());

        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: Some(Pipeline::start(
                &path_a,
                Duration::from_millis(50),
                config::DEFAULT_MAX_REQUESTS as usize,
            )),
            path: Some(path_a.clone()),
            install_folder: Some(install.clone()),
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
        }));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        // Switch to a log file folder whose log file does not exist: the app
        // still switches and the tail reports Disconnected, retrying until
        // the file appears.
        handle_settings_save(
            &window,
            &app,
            &charts,
            &test_gpu(),
            Some(&cp),
            500,
            5_000,
            1000,
            &install.display().to_string(),
            &folder_b.display().to_string(),
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
            config::load(&cp).log_file_folder,
            folder_b.display().to_string()
        );

        wait_until(|| {
            apply_latest(&window, &app, &charts);
            window.get_connection_status() == "Disconnected"
        });
        assert_eq!(window.get_connection_status(), "Disconnected");

        // When the file appears, the tail recovers to Live and backfills it.
        std::fs::create_dir_all(missing.parent().unwrap()).unwrap();
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
    fn startup_start_with_valid_folder_starts_pipeline_and_hides_screen() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("ninfer");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::File::create(folder.join(startup::ninfer_serve_exe_name())).unwrap();

        let window = MainWindow::new().unwrap();
        window.set_startup_visible(true);
        window.set_startup_folder(folder.display().to_string().into());
        window.set_startup_start_enabled(true);

        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
        assert_eq!(app.lock().unwrap().install_folder, Some(folder.clone()));
        assert_eq!(
            window.get_install_folder_full(),
            folder.display().to_string()
        );
    }

    #[test]
    fn startup_start_restarts_the_persisted_configuration() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("ninfer");
        std::fs::create_dir_all(&folder).unwrap();
        make_test_ninfer_serve(&folder);
        let cp = dir.path().join("config.json");
        {
            let cfg = config::Config {
                log_file_folder: dir.path().display().to_string(),
                running_config: Some("a".to_owned()),
                ..Default::default()
            };
            let _ = config::save(&cp, &cfg);
        }

        let window = MainWindow::new().unwrap();
        window.set_startup_visible(true);
        window.set_startup_folder(folder.display().to_string().into());
        window.set_startup_start_enabled(true);

        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", test_ninfer_serve_command_line())],
            Some(0),
        )));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));

        handle_startup_start(&window, &app, &charts, Some(cp.as_path()));

        assert!(!window.get_startup_visible());
        let a = app.lock().unwrap();
        assert!(a.configs[0].running);
        assert!(a.control_status.starts_with("Started."));
        assert_eq!(config::load(&cp).running_config, Some("a".to_owned()));
        let id = a.configs[0].id;
        drop(a);
        let mut a = app.lock().unwrap();
        stop_config_process(&mut a, id);
    }

    #[test]
    fn startup_start_with_invalid_folder_does_nothing() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("missing");

        let window = MainWindow::new().unwrap();
        window.set_startup_visible(true);
        window.set_startup_folder(folder.display().to_string().into());
        window.set_startup_start_enabled(true);

        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
        assert_eq!(app.lock().unwrap().install_folder, None);
    }

    #[test]
    fn startup_start_with_empty_folder_does_nothing() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window.set_startup_visible(true);
        window.set_startup_folder("".into());
        window.set_startup_start_enabled(true);

        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
    fn startup_folder_edited_updates_start_enabled() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("ninfer");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::File::create(folder.join(startup::ninfer_serve_exe_name())).unwrap();

        let window = MainWindow::new().unwrap();
        window.set_startup_visible(true);

        handle_startup_folder_edited(&window, "");
        assert!(
            !window.get_startup_start_enabled(),
            "an empty folder disables Start"
        );

        handle_startup_folder_edited(&window, &folder.display().to_string());
        assert!(
            window.get_startup_start_enabled(),
            "a valid folder enables Start"
        );

        handle_startup_folder_edited(&window, "missing");
        assert!(
            !window.get_startup_start_enabled(),
            "an invalid folder disables Start"
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
            352.0,
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
        handle_column_changed(&window, &cols, 2, 300.0);
        assert_eq!(window.get_col_model(), 300.0);
        assert_eq!(window.get_col_prompt(), 60.0 + (352.0 - 300.0));
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
        assert_eq!(saved.model, 352);
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
            model_at_default, 352,
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
    fn tab_changed_prefills_from_config_and_current_file() {
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
            path: None,
            install_folder: Some(PathBuf::from("C:/ninfer")),
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
        }));
        let update = test_update();
        {
            let mut u = update.lock().unwrap();
            u.enabled = false;
            u.period_days = 9;
        }
        handle_tab_changed(&window, &app, &update, Some(&cp), 2);
        assert_eq!(window.get_active_tab(), 2);
        assert_eq!(window.get_settings_log_poll_ms(), 200);
        assert_eq!(window.get_settings_gpu_poll_ms(), 3_000);
        assert_eq!(window.get_settings_max_requests(), 250);
        assert_eq!(window.get_settings_install_folder(), "C:/ninfer");
        assert!(!window.get_settings_update_check_enabled());
        assert_eq!(window.get_settings_update_check_days(), 9);
    }

    #[test]
    fn tab_changed_without_config_uses_defaults() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
        }));
        handle_tab_changed(&window, &app, &test_update(), None, 2);
        assert_eq!(window.get_active_tab(), 2);
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
        assert_eq!(window.get_settings_install_folder(), "");
        assert_eq!(
            window.get_settings_log_file_folder(),
            config::default_log_file_folder()
        );
        assert!(window.get_settings_update_check_enabled());
        assert_eq!(
            window.get_settings_update_check_days(),
            update_check::DEFAULT_PERIOD_DAYS as i32
        );
    }

    #[test]
    fn tab_changed_on_active_settings_tab_keeps_edits() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(
            &cp,
            r#"{"log_poll_interval_ms":200,"gpu_poll_interval_ms":3000,"max_requests":250}"#,
        )
        .unwrap();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
        }));
        // Switching to the Settings tab from the default (Control) tab
        // pre-fills the fields from the saved values.
        handle_tab_changed(&window, &app, &test_update(), Some(&cp), 2);
        assert_eq!(window.get_active_tab(), 2);
        assert_eq!(window.get_settings_log_poll_ms(), 200);
        assert_eq!(window.get_settings_max_requests(), 250);
        // The user edits the fields in the tab.
        window.set_settings_log_poll_ms(400);
        window.set_settings_max_requests(900);
        // Clicking the already-active Settings tab is a no-op: the edits
        // survive.
        handle_tab_changed(&window, &app, &test_update(), Some(&cp), 2);
        assert_eq!(window.get_active_tab(), 2);
        assert_eq!(window.get_settings_log_poll_ms(), 400);
        assert_eq!(window.get_settings_max_requests(), 900);
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
            "",
            "F",
            true,
            3,
            &test_update(),
        );
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
    fn settings_discard_refills_defaults_without_config() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(App {
            pipeline: None,
            path: None,
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
        }));
        // Simulate edits made in the Settings tab.
        window.set_settings_log_poll_ms(500);
        window.set_settings_install_folder("edited".into());
        window.set_nvidia_popup_status("NVIDIA game popup disabled.".into());
        handle_settings_discard(&window, &app, &test_update(), None);
        assert_eq!(
            window.get_settings_log_poll_ms(),
            Config::default().log_poll_interval_ms as i32
        );
        assert_eq!(window.get_settings_install_folder(), "");
        assert_eq!(window.get_nvidia_popup_status(), "");
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
    fn manual_update_check_success_persists_and_sets_available() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"log_poll_interval_ms":200}"#).unwrap();
        let update = test_update();
        let now = 1_700_000_000_000u64;
        let outcome =
            run_manual_update_check_with(&update, Some(&cp), now, || Ok(Some(test_release())));
        assert!(matches!(outcome, Ok(Some(_))));
        let cfg = config::load(&cp);
        assert_eq!(cfg.last_update_check_unix_ms, Some(now));
        assert_eq!(cfg.log_poll_interval_ms, 200, "the other settings are kept");
        let u = update.lock().unwrap();
        assert_eq!(u.last_check_unix_ms, Some(now));
        assert_eq!(u.last_attempt_unix_ms, Some(now));
        assert_eq!(
            u.available.as_ref().map(|r| r.version.as_str()),
            Some("0.3.0")
        );
    }

    #[test]
    fn manual_update_check_ignores_schedule_and_throttle() {
        let now = 1_700_000_000_000u64;
        let update = Arc::new(Mutex::new(UpdateState {
            enabled: true,
            period_days: 3,
            last_check_unix_ms: Some(now),
            last_attempt_unix_ms: Some(now),
            available: None,
        }));
        let calls = std::cell::Cell::new(0);
        let outcome = run_manual_update_check_with(&update, None, now + 1_000, || {
            calls.set(calls.get() + 1);
            Ok(Some(test_release()))
        });
        assert!(
            matches!(outcome, Ok(Some(_))),
            "the manual check runs even when not due and throttled"
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(update.lock().unwrap().last_check_unix_ms, Some(now + 1_000));
    }

    #[test]
    fn manual_update_check_runs_when_disabled() {
        let update = Arc::new(Mutex::new(UpdateState {
            enabled: false,
            period_days: 3,
            last_check_unix_ms: None,
            last_attempt_unix_ms: None,
            available: None,
        }));
        let outcome = run_manual_update_check_with(&update, None, 1_700_000_000_000, || {
            Ok(Some(test_release()))
        });
        assert!(matches!(outcome, Ok(Some(_))));
        assert_eq!(
            update
                .lock()
                .unwrap()
                .available
                .as_ref()
                .map(|r| r.version.as_str()),
            Some("0.3.0"),
            "a manual check mirrors the result even when the automatic check is disabled"
        );
    }

    #[test]
    fn manual_update_check_failure_keeps_state_and_throttles() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        let update = Arc::new(Mutex::new(UpdateState {
            enabled: true,
            period_days: 3,
            last_check_unix_ms: Some(1_600_000_000_000),
            last_attempt_unix_ms: None,
            available: Some(test_release()),
        }));
        let now = 1_700_000_000_000u64;
        let outcome = run_manual_update_check_with(&update, Some(&cp), now, || {
            Err(update_check::CheckError::Http("timeout".to_owned()))
        });
        assert!(outcome.is_err());
        let u = update.lock().unwrap();
        assert_eq!(
            u.last_check_unix_ms,
            Some(1_600_000_000_000),
            "a failed check does not record a check"
        );
        assert_eq!(
            u.last_attempt_unix_ms,
            Some(now),
            "the failed attempt throttles the next automatic check"
        );
        assert_eq!(
            u.available.as_ref().map(|r| r.version.as_str()),
            Some("0.3.0"),
            "the previously found release is kept"
        );
        assert!(
            config::load(&cp).last_update_check_unix_ms.is_none(),
            "nothing persisted"
        );
    }

    #[test]
    fn manual_update_check_success_without_new_release_clears_available() {
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
        let outcome = run_manual_update_check_with(&update, Some(&cp), now, || Ok(None));
        assert!(matches!(outcome, Ok(None)));
        let u = update.lock().unwrap();
        assert_eq!(u.last_check_unix_ms, Some(now));
        assert!(u.available.is_none(), "no newer release -> not available");
        assert_eq!(config::load(&cp).last_update_check_unix_ms, Some(now));
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
    fn tab_changed_prefills_update_check_settings() {
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
        }));
        let update = test_update();
        {
            let mut u = update.lock().unwrap();
            u.enabled = false;
            u.period_days = 5;
        }
        handle_tab_changed(&window, &app, &update, Some(&cp), 2);
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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
            install_folder: None,
            configs: Vec::new(),
            selected_config: None,
            next_config_id: 1,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
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

    /// An `App` with the given (name, command line) configurations and
    /// selection, for the Control-tab tests (v0.3 M3).
    fn test_app_with_configs(configs: Vec<(&str, &str)>, selected: Option<usize>) -> App {
        let built: Vec<ControlConfig> = configs
            .into_iter()
            .enumerate()
            .map(|(i, (name, command_line))| ControlConfig {
                id: (i as u64) + 1,
                name: name.to_owned(),
                command_line: command_line.to_owned(),
                running: false,
                error: None,
            })
            .collect();
        let next_config_id = built.len() as u64 + 1;
        App {
            pipeline: None,
            path: None,
            install_folder: None,
            configs: built,
            selected_config: selected,
            next_config_id,
            control_status: String::new(),
            processes: std::collections::HashMap::new(),
        }
    }

    /// A short-lived child that exits on its own (v0.3 M4 tests).
    #[cfg(windows)]
    fn spawn_exiting_child() -> std::process::Child {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap()
    }

    #[cfg(not(windows))]
    fn spawn_exiting_child() -> std::process::Child {
        std::process::Command::new("true").spawn().unwrap()
    }

    /// A child that stays alive long enough to be killed (v0.3 M4 tests).
    #[cfg(windows)]
    fn spawn_long_lived_child() -> std::process::Child {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        std::process::Command::new("timeout")
            .args(["/t", "10", "/nobreak"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap()
    }

    #[cfg(not(windows))]
    fn spawn_long_lived_child() -> std::process::Child {
        std::process::Command::new("sleep")
            .args(["10"])
            .spawn()
            .unwrap()
    }

    /// A child that prints to stderr and exits with a non-zero code (v0.3 M4
    /// tests for the captured console tail).
    #[cfg(windows)]
    fn spawn_failing_child() -> std::process::Child {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        std::process::Command::new("cmd")
            .args(["/c", "echo boom 1>&2 & exit 3"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap()
    }

    #[cfg(not(windows))]
    fn spawn_failing_child() -> std::process::Child {
        std::process::Command::new("sh")
            .args(["-c", "echo boom >&2; exit 3"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn make_test_ninfer_serve(folder: &Path) {
        let program = process_control::program_path(folder);
        #[cfg(windows)]
        {
            std::fs::copy("C:\\Windows\\System32\\timeout.exe", &program).unwrap();
        }
        #[cfg(not(windows))]
        {
            std::fs::copy("/bin/sleep", &program).unwrap();
        }
    }

    fn test_ninfer_serve_command_line() -> &'static str {
        if cfg!(windows) {
            "/t 10 /nobreak"
        } else {
            "10"
        }
    }

    #[test]
    fn poll_processes_detects_an_exited_child() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));
        let config_id = {
            let mut a = app.lock().unwrap();
            let id = a.configs[0].id;
            a.processes
                .insert(id, TrackedProcess::new(spawn_exiting_child()));
            a.configs[0].running = true;
            id
        };
        // Reap the child so the poll sees a definite exit.
        {
            let mut a = app.lock().unwrap();
            if let Some(tracked) = a.processes.get_mut(&config_id) {
                let _ = tracked.child.wait();
            }
        }
        poll_processes(&window, &app, None);
        let a = app.lock().unwrap();
        assert!(a.processes.is_empty());
        assert!(!a.configs[0].running);
        assert!(window.get_control_start_enabled());
        assert!(!window.get_control_stop_enabled());
    }

    #[test]
    fn poll_processes_reaps_a_child_that_exits_on_its_own() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));
        {
            let mut a = app.lock().unwrap();
            let id = a.configs[0].id;
            a.processes
                .insert(id, TrackedProcess::new(spawn_exiting_child()));
            a.configs[0].running = true;
        }
        // Let the child exit on its own, then poll until it is reaped — this
        // exercises the `Ok(Some(_))` branch (a process that exits on its
        // own), not the `Err(_)` safety net.
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            poll_processes(&window, &app, None);
            if app.lock().unwrap().processes.is_empty() {
                break;
            }
        }
        let a = app.lock().unwrap();
        assert!(a.processes.is_empty());
        assert!(!a.configs[0].running);
    }

    #[test]
    fn poll_processes_marks_a_failed_child_red_and_shows_its_output() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));
        {
            let mut a = app.lock().unwrap();
            let id = a.configs[0].id;
            a.processes
                .insert(id, TrackedProcess::new(spawn_failing_child()));
            a.configs[0].running = true;
        }
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            poll_processes(&window, &app, None);
            if app.lock().unwrap().processes.is_empty() {
                break;
            }
        }
        let a = app.lock().unwrap();
        assert!(a.processes.is_empty());
        assert!(!a.configs[0].running);
        let error = a.configs[0].error.clone().unwrap();
        assert!(error.starts_with("Error: the process exited with exit code 3"));
        assert!(error.contains("boom"));
        assert_eq!(a.control_status, error);
        assert!(window.get_control_status_error());
    }

    #[test]
    fn poll_processes_keeps_a_running_child() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));
        let config_id = {
            let mut a = app.lock().unwrap();
            let id = a.configs[0].id;
            a.processes
                .insert(id, TrackedProcess::new(spawn_long_lived_child()));
            a.configs[0].running = true;
            id
        };
        poll_processes(&window, &app, None);
        let mut a = app.lock().unwrap();
        assert_eq!(a.processes.len(), 1);
        assert!(a.configs[0].running);
        stop_config_process(&mut a, config_id);
    }

    #[test]
    fn stop_config_process_kills_a_running_child() {
        let mut app = test_app_with_configs(vec![("a", "x")], Some(0));
        let config_id = app.configs[0].id;
        app.processes
            .insert(config_id, TrackedProcess::new(spawn_long_lived_child()));
        app.configs[0].running = true;
        let started = Instant::now();
        stop_config_process(&mut app, config_id);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(app.processes.is_empty());
        assert!(!app.configs[0].running);
    }

    #[test]
    fn stop_all_config_processes_stops_every_running_child() {
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", "x"), ("b", "y")],
            Some(0),
        )));
        let ids = {
            let mut a = app.lock().unwrap();
            let ids: Vec<u64> = a.configs.iter().map(|c| c.id).collect();
            for id in &ids {
                a.processes
                    .insert(*id, TrackedProcess::new(spawn_long_lived_child()));
            }
            for config in &mut a.configs {
                config.running = true;
            }
            ids
        };
        let started = Instant::now();
        stop_all_config_processes(&app);
        assert!(started.elapsed() < Duration::from_secs(2));
        let a = app.lock().unwrap();
        assert!(a.processes.is_empty());
        for config in &a.configs {
            assert!(!config.running);
        }
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn stop_all_config_processes_does_not_clear_the_persisted_running_config() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"running_config":"a"}"#).unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));
        {
            let mut a = app.lock().unwrap();
            let id = a.configs[0].id;
            a.processes
                .insert(id, TrackedProcess::new(spawn_long_lived_child()));
            a.configs[0].running = true;
        }
        stop_all_config_processes(&app);
        assert_eq!(config::load(&cp).running_config, Some("a".to_owned()));
    }

    #[test]
    fn start_persists_the_running_configuration() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let install_folder = dir.path().join("ninfer");
        std::fs::create_dir_all(&install_folder).unwrap();
        make_test_ninfer_serve(&install_folder);
        let config_path = dir.path().join("config.json");
        {
            let cfg = config::Config {
                log_file_folder: dir.path().display().to_string(),
                ..Default::default()
            };
            let _ = config::save(&config_path, &cfg);
        }
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", test_ninfer_serve_command_line())],
            Some(0),
        )));
        {
            let mut a = app.lock().unwrap();
            a.install_folder = Some(install_folder.clone());
        }
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        handle_control_process_action(
            &window,
            &app,
            &charts,
            Some(config_path.as_path()),
            ProcessAction::Start,
        );
        let a = app.lock().unwrap();
        assert!(a.configs[0].running);
        assert!(a.control_status.starts_with("Started."));
        assert_eq!(
            config::load(&config_path).running_config,
            Some("a".to_owned())
        );
        let id = a.configs[0].id;
        drop(a);
        let mut a = app.lock().unwrap();
        stop_config_process(&mut a, id);
    }

    #[test]
    fn restart_shows_the_restarted_status() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let install_folder = dir.path().join("ninfer");
        std::fs::create_dir_all(&install_folder).unwrap();
        make_test_ninfer_serve(&install_folder);
        let config_path = dir.path().join("config.json");
        {
            let cfg = config::Config {
                log_file_folder: dir.path().display().to_string(),
                ..Default::default()
            };
            let _ = config::save(&config_path, &cfg);
        }
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", test_ninfer_serve_command_line())],
            Some(0),
        )));
        {
            let mut a = app.lock().unwrap();
            a.install_folder = Some(install_folder.clone());
        }
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        handle_control_process_action(
            &window,
            &app,
            &charts,
            Some(config_path.as_path()),
            ProcessAction::Start,
        );
        handle_control_process_action(
            &window,
            &app,
            &charts,
            Some(config_path.as_path()),
            ProcessAction::Restart,
        );
        let a = app.lock().unwrap();
        assert!(a.configs[0].running);
        assert!(a.control_status.starts_with("Restarted."));
        let id = a.configs[0].id;
        drop(a);
        let mut a = app.lock().unwrap();
        stop_config_process(&mut a, id);
    }

    #[test]
    fn stop_clears_the_persisted_running_configuration() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"running_config":"a"}"#).unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));
        {
            let mut a = app.lock().unwrap();
            a.configs[0].running = true;
        }
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        handle_control_process_action(
            &window,
            &app,
            &charts,
            Some(cp.as_path()),
            ProcessAction::Stop,
        );
        let a = app.lock().unwrap();
        assert!(!a.configs[0].running);
        assert_eq!(config::load(&cp).running_config, None);
    }

    #[test]
    fn stop_of_another_configuration_keeps_the_persisted_running_config() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"running_config":"a"}"#).unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", "x"), ("b", "y")],
            Some(1),
        )));
        {
            let mut a = app.lock().unwrap();
            a.configs[0].running = true;
            a.configs[1].running = true;
        }
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        handle_control_process_action(
            &window,
            &app,
            &charts,
            Some(cp.as_path()),
            ProcessAction::Stop,
        );
        let a = app.lock().unwrap();
        assert!(a.configs[0].running);
        assert!(!a.configs[1].running);
        assert_eq!(config::load(&cp).running_config, Some("a".to_owned()));
    }

    #[test]
    fn poll_processes_clears_the_persisted_running_configuration_on_exit() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"running_config":"a"}"#).unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));
        {
            let mut a = app.lock().unwrap();
            let id = a.configs[0].id;
            a.processes
                .insert(id, TrackedProcess::new(spawn_exiting_child()));
            a.configs[0].running = true;
        }
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            poll_processes(&window, &app, Some(cp.as_path()));
            if app.lock().unwrap().processes.is_empty() {
                break;
            }
        }
        let a = app.lock().unwrap();
        assert!(a.processes.is_empty());
        assert!(!a.configs[0].running);
        assert_eq!(config::load(&cp).running_config, None);
    }

    #[test]
    fn handle_control_delete_clears_the_persisted_running_configuration() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"running_config":"a"}"#).unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", "x"), ("b", "y")],
            Some(0),
        )));
        handle_control_delete(&window, &app, Some(cp.as_path()));
        assert_eq!(config::load(&cp).running_config, None);
    }

    #[test]
    fn restart_running_config_starts_the_persisted_configuration() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"running_config":"A"}"#).unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", "--model x"), ("b", "y")],
            None,
        )));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        restart_running_config(&window, &app, &charts, Some(cp.as_path()));
        let a = app.lock().unwrap();
        assert_eq!(a.control_status, "Error: no installation folder configured");
        assert!(!a.configs[0].running);
        assert!(
            a.configs[0]
                .error
                .as_deref()
                .is_some_and(|e| e == "Error: no installation folder configured")
        );
        assert!(a.configs[1].error.is_none());
        assert_eq!(config::load(&cp).running_config, Some("A".to_owned()));
    }

    #[test]
    fn restart_running_config_is_a_no_op_without_a_match() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"running_config":"missing"}"#).unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        restart_running_config(&window, &app, &charts, Some(cp.as_path()));
        let a = app.lock().unwrap();
        assert_eq!(a.control_status, "");
        assert!(a.configs[0].error.is_none());
        assert_eq!(config::load(&cp).running_config, Some("missing".to_owned()));
    }

    #[test]
    fn restart_running_config_is_a_no_op_when_already_running() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("config.json");
        std::fs::write(&cp, r#"{"running_config":"a"}"#).unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));
        {
            let mut a = app.lock().unwrap();
            a.configs[0].running = true;
        }
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        restart_running_config(&window, &app, &charts, Some(cp.as_path()));
        let a = app.lock().unwrap();
        assert_eq!(a.control_status, "");
        assert!(a.configs[0].error.is_none());
    }

    #[test]
    fn start_without_an_install_folder_shows_the_error() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", "--model x")],
            Some(0),
        )));
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        handle_control_process_action(&window, &app, &charts, None, ProcessAction::Start);
        let a = app.lock().unwrap();
        assert_eq!(a.control_status, "Error: no installation folder configured");
        assert!(a.processes.is_empty());
        assert!(!a.configs[0].running);
        assert_eq!(
            a.configs[0].error.as_deref(),
            Some("Error: no installation folder configured")
        );
    }

    #[test]
    fn start_with_an_empty_command_line_shows_the_error() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", "   ")],
            Some(0),
        )));
        {
            let mut a = app.lock().unwrap();
            a.install_folder = Some(PathBuf::from("C:/ninfer"));
        }
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        handle_control_process_action(&window, &app, &charts, None, ProcessAction::Start);
        let a = app.lock().unwrap();
        assert_eq!(a.control_status, "Error: the command line is empty");
        assert!(a.processes.is_empty());
        assert!(!a.configs[0].running);
        assert_eq!(
            a.configs[0].error.as_deref(),
            Some("Error: the command line is empty")
        );
    }

    #[test]
    fn start_with_a_missing_executable_shows_the_error() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        {
            let cfg = config::Config {
                log_file_folder: dir.path().display().to_string(),
                ..Default::default()
            };
            let _ = config::save(&config_path, &cfg);
        }
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", "--model x")],
            Some(0),
        )));
        {
            let mut a = app.lock().unwrap();
            a.install_folder = Some(dir.path().to_path_buf());
        }
        let charts = Arc::new(Mutex::new(new_chart_state(
            300_000,
            config::DEFAULT_MAX_REQUESTS,
        )));
        handle_control_process_action(
            &window,
            &app,
            &charts,
            Some(config_path.as_path()),
            ProcessAction::Start,
        );
        let a = app.lock().unwrap();
        assert!(a.control_status.starts_with("Error: failed to start"));
        assert!(a.processes.is_empty());
        assert!(!a.configs[0].running);
        assert!(
            a.configs[0]
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("Error: failed to start"))
        );
    }

    #[test]
    fn delete_previous_log_file_removes_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("server.requests.jsonl");
        assert!(delete_previous_log_file(&log_path).is_none());
        std::fs::write(&log_path, b"old").unwrap();
        assert!(delete_previous_log_file(&log_path).is_none());
        assert!(!log_path.exists());
    }

    #[test]
    fn delete_previous_log_file_reports_a_failed_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("server.requests.jsonl");
        std::fs::create_dir(&log_path).unwrap();
        let error = delete_previous_log_file(&log_path).unwrap();
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn load_control_configs_assigns_ids_and_next_id() {
        let entries = vec![
            ConfigEntry {
                name: "a".to_owned(),
                command_line: "x".to_owned(),
            },
            ConfigEntry {
                name: "b".to_owned(),
                command_line: "y".to_owned(),
            },
        ];
        let (configs, next_id) = load_control_configs(&entries);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].id, 1);
        assert_eq!(configs[1].id, 2);
        assert_eq!(configs[0].name, "a");
        assert_eq!(configs[1].command_line, "y");
        assert_eq!(next_id, 3);
    }

    #[test]
    fn apply_control_edit_updates_and_reports_saved() {
        let mut app = test_app_with_configs(vec![("a", "x")], Some(0));
        let status = apply_control_edit(&mut app, None, "b", "y");
        assert_eq!(status.as_deref(), Some("Saved."));
        assert_eq!(app.configs[0].name, "b");
        assert_eq!(app.configs[0].command_line, "y");
    }

    #[test]
    fn apply_control_edit_updates_the_persisted_running_config_on_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"running_config":"a"}"#).unwrap();
        let mut app = test_app_with_configs(vec![("a", "x"), ("b", "y")], Some(0));
        let status = apply_control_edit(&mut app, Some(path.as_path()), "b-renamed", "x");
        assert_eq!(status.as_deref(), Some("Saved."));
        assert_eq!(
            config::load(&path).running_config,
            Some("b-renamed".to_owned())
        );
    }

    #[test]
    fn apply_control_edit_keeps_the_persisted_running_config_for_another_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"running_config":"a"}"#).unwrap();
        let mut app = test_app_with_configs(vec![("a", "x"), ("b", "y")], Some(1));
        let status = apply_control_edit(&mut app, Some(path.as_path()), "c", "y");
        assert_eq!(status.as_deref(), Some("Saved."));
        assert_eq!(config::load(&path).running_config, Some("a".to_owned()));
    }

    #[test]
    fn apply_control_edit_command_line_only_keeps_the_persisted_running_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"running_config":"a"}"#).unwrap();
        let mut app = test_app_with_configs(vec![("a", "x")], Some(0));
        let status = apply_control_edit(&mut app, Some(path.as_path()), "a", "y");
        assert_eq!(status.as_deref(), Some("Saved."));
        assert_eq!(config::load(&path).running_config, Some("a".to_owned()));
    }

    #[test]
    fn apply_control_edit_is_a_no_op_when_unchanged() {
        let mut app = test_app_with_configs(vec![("a", "x")], Some(0));
        // The stored name is trimmed, so a name that trims to the stored name
        // with the same command line is a no-op.
        let status = apply_control_edit(&mut app, None, "  a  ", "x");
        assert_eq!(status, None);
        assert_eq!(app.configs[0].name, "a");
        assert_eq!(app.configs[0].command_line, "x");
    }

    #[test]
    fn apply_control_edit_rejects_an_empty_name() {
        let mut app = test_app_with_configs(vec![("a", "x")], Some(0));
        let status = apply_control_edit(&mut app, None, "   ", "y");
        assert_eq!(status.as_deref(), Some("Error: the name is empty"));
        assert_eq!(app.configs[0].name, "a");
        assert_eq!(app.configs[0].command_line, "x");
    }

    #[test]
    fn apply_control_edit_rejects_a_duplicate_name_case_insensitively() {
        let mut app = test_app_with_configs(vec![("a", "x"), ("b", "y")], Some(1));
        let status = apply_control_edit(&mut app, None, "A", "y");
        assert_eq!(status.as_deref(), Some("Error: the name must be unique"));
        assert_eq!(app.configs[1].name, "b");
        assert_eq!(app.configs[1].command_line, "y");
    }

    #[test]
    fn apply_control_edit_rejects_an_empty_command_line() {
        let mut app = test_app_with_configs(vec![("a", "x")], Some(0));
        let status = apply_control_edit(&mut app, None, "b", "   ");
        assert_eq!(status.as_deref(), Some("Error: the command line is empty"));
        assert_eq!(app.configs[0].name, "a");
        assert_eq!(app.configs[0].command_line, "x");
    }

    #[test]
    fn apply_control_edit_is_a_no_op_without_a_selection() {
        let mut app = test_app_with_configs(vec![("a", "x")], None);
        let status = apply_control_edit(&mut app, None, "b", "y");
        assert_eq!(status, None);
        assert_eq!(app.configs[0].name, "a");
    }

    #[test]
    fn refresh_control_enablement_when_nothing_is_running() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = test_app_with_configs(vec![("a", "x"), ("b", "  ")], Some(0));
        refresh_control(&window, &app);
        assert_eq!(window.get_control_selected(), 0);
        assert!(window.get_control_start_enabled());
        assert!(!window.get_control_restart_enabled());
        assert!(!window.get_control_stop_enabled());
        assert!(window.get_control_delete_enabled());
    }

    #[test]
    fn refresh_control_enablement_when_a_config_is_running() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let mut app = test_app_with_configs(vec![("a", "x"), ("b", "y")], Some(0));
        app.configs[0].running = true;
        refresh_control(&window, &app);
        assert_eq!(window.get_control_selected(), 0);
        assert!(!window.get_control_start_enabled());
        assert!(window.get_control_restart_enabled());
        assert!(window.get_control_stop_enabled());
        assert!(window.get_control_delete_enabled());
    }

    #[test]
    fn refresh_control_stop_disabled_without_a_selection_when_running() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let mut app = test_app_with_configs(vec![("a", "x")], None);
        app.configs[0].running = true;
        refresh_control(&window, &app);
        assert_eq!(window.get_control_selected(), -1);
        assert!(!window.get_control_start_enabled());
        assert!(!window.get_control_restart_enabled());
        assert!(!window.get_control_stop_enabled());
        assert!(!window.get_control_delete_enabled());
    }

    #[test]
    fn refresh_control_stop_disabled_when_the_selected_config_is_not_running() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let mut app = test_app_with_configs(vec![("a", "x"), ("b", "y")], Some(1));
        app.configs[0].running = true;
        refresh_control(&window, &app);
        assert_eq!(window.get_control_selected(), 1);
        assert!(!window.get_control_start_enabled());
        assert!(window.get_control_restart_enabled());
        assert!(!window.get_control_stop_enabled());
        assert!(window.get_control_delete_enabled());
    }

    #[test]
    fn refresh_control_disables_all_without_a_selection() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = test_app_with_configs(vec![("a", "x")], None);
        refresh_control(&window, &app);
        assert_eq!(window.get_control_selected(), -1);
        assert!(!window.get_control_start_enabled());
        assert!(!window.get_control_restart_enabled());
        assert!(!window.get_control_stop_enabled());
        assert!(!window.get_control_delete_enabled());
    }

    #[test]
    fn load_fields_loads_the_selected_config() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = test_app_with_configs(vec![("a", "x"), ("b", "y")], Some(1));
        load_fields(&window, &app);
        assert_eq!(window.get_control_name().as_str(), "b");
        assert_eq!(window.get_control_command_line().as_str(), "y");
    }

    #[test]
    fn load_fields_clears_without_a_selection() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let app = test_app_with_configs(vec![("a", "x")], None);
        load_fields(&window, &app);
        assert_eq!(window.get_control_name().as_str(), "");
        assert_eq!(window.get_control_command_line().as_str(), "");
    }

    #[test]
    fn persist_configs_writes_the_configurations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = test_app_with_configs(vec![("a", "x"), ("b", "y")], Some(0));
        persist_configs(&app, Some(path.as_path()));
        let cfg = config::load(&path);
        assert_eq!(cfg.configs.len(), 2);
        assert_eq!(cfg.configs[0].name, "a");
        assert_eq!(cfg.configs[1].command_line, "y");
    }

    #[test]
    fn handle_control_add_creates_selects_and_persists() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![], None)));

        handle_control_add(&window, &app, Some(path.as_path()));

        {
            let a = app.lock().unwrap();
            assert_eq!(a.configs.len(), 1);
            assert_eq!(a.configs[0].name, "Config 1");
            assert_eq!(a.configs[0].command_line, "");
            assert_eq!(a.selected_config, Some(0));
            assert_eq!(a.control_status, "Added.");
        }
        assert_eq!(window.get_control_name().as_str(), "Config 1");
        assert_eq!(window.get_control_command_line().as_str(), "");
        assert_eq!(window.get_control_status().as_str(), "Added.");
        assert_eq!(window.get_control_focus_name(), 1);

        let cfg = config::load(&path);
        assert_eq!(cfg.configs.len(), 1);
        assert_eq!(cfg.configs[0].name, "Config 1");

        handle_control_add(&window, &app, Some(path.as_path()));
        {
            let a = app.lock().unwrap();
            assert_eq!(a.configs.len(), 2);
            assert_eq!(a.configs[1].name, "Config 2");
            assert_eq!(a.selected_config, Some(1));
        }
        assert_eq!(window.get_control_focus_name(), 2);
    }

    #[test]
    fn handle_control_delete_removes_selected_and_persists() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", "x"), ("b", "y")],
            Some(0),
        )));

        handle_control_delete(&window, &app, Some(path.as_path()));

        {
            let a = app.lock().unwrap();
            assert_eq!(a.configs.len(), 1);
            assert_eq!(a.configs[0].name, "b");
            assert_eq!(a.selected_config, None);
            assert_eq!(a.control_status, "Deleted.");
        }
        assert_eq!(window.get_control_name().as_str(), "");
        assert_eq!(window.get_control_command_line().as_str(), "");
        assert_eq!(window.get_control_status().as_str(), "Deleted.");

        let cfg = config::load(&path);
        assert_eq!(cfg.configs.len(), 1);
        assert_eq!(cfg.configs[0].name, "b");
    }

    #[test]
    fn handle_control_delete_without_a_selection_is_a_no_op() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], None)));

        handle_control_delete(&window, &app, Some(path.as_path()));

        {
            let a = app.lock().unwrap();
            assert_eq!(a.configs.len(), 1);
            assert_eq!(a.selected_config, None);
            assert_eq!(a.control_status, "");
        }
    }

    #[test]
    fn handle_control_config_selected_commits_pending_edit_then_selects() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", "x"), ("b", "y")],
            Some(0),
        )));

        window.set_control_name("a-edited".into());
        window.set_control_command_line("x-edited".into());

        handle_control_config_selected(&window, &app, Some(path.as_path()), 1);

        {
            let a = app.lock().unwrap();
            assert_eq!(a.configs[0].name, "a-edited");
            assert_eq!(a.configs[0].command_line, "x-edited");
            assert_eq!(a.configs[1].name, "b");
            assert_eq!(a.selected_config, Some(1));
        }
        assert_eq!(window.get_control_name().as_str(), "b");
        assert_eq!(window.get_control_command_line().as_str(), "y");

        let cfg = config::load(&path);
        assert_eq!(cfg.configs[0].name, "a-edited");
        assert_eq!(cfg.configs[0].command_line, "x-edited");
    }

    #[test]
    fn handle_control_edit_paused_saves_a_valid_edit() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));

        handle_control_edit_paused(
            &window,
            &app,
            Some(path.as_path()),
            "  b  ".into(),
            "y".into(),
        );

        {
            let a = app.lock().unwrap();
            assert_eq!(a.configs[0].name, "b");
            assert_eq!(a.configs[0].command_line, "y");
            assert_eq!(a.control_status, "Saved.");
        }
        assert_eq!(window.get_control_status().as_str(), "Saved.");
        assert_eq!(window.get_control_name().as_str(), "b");

        let cfg = config::load(&path);
        assert_eq!(cfg.configs[0].name, "b");
        assert_eq!(cfg.configs[0].command_line, "y");
    }

    #[test]
    fn handle_control_edit_paused_rejects_a_duplicate_name() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = Arc::new(Mutex::new(test_app_with_configs(
            vec![("a", "x"), ("b", "y")],
            Some(1),
        )));

        let before = std::fs::read_to_string(&path).unwrap_or_default();
        handle_control_edit_paused(&window, &app, Some(path.as_path()), "A".into(), "y".into());

        {
            let a = app.lock().unwrap();
            assert_eq!(a.configs[1].name, "b");
            assert_eq!(a.control_status, "Error: the name must be unique");
        }
        assert_eq!(
            window.get_control_status().as_str(),
            "Error: the name must be unique"
        );
        let after = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(before, after);
    }

    #[test]
    fn handle_control_edit_paused_rejects_an_empty_command_line() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));

        handle_control_edit_paused(
            &window,
            &app,
            Some(path.as_path()),
            "b".into(),
            "   ".into(),
        );

        {
            let a = app.lock().unwrap();
            assert_eq!(a.configs[0].name, "a");
            assert_eq!(a.configs[0].command_line, "x");
            assert_eq!(a.control_status, "Error: the command line is empty");
        }
        assert_eq!(
            window.get_control_status().as_str(),
            "Error: the command line is empty"
        );
    }

    #[test]
    fn handle_tab_changed_commits_pending_edit_on_leaving_control() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![("a", "x")], Some(0))));
        let update = Arc::new(Mutex::new(UpdateState {
            enabled: false,
            period_days: 3,
            last_check_unix_ms: None,
            last_attempt_unix_ms: None,
            available: None,
        }));

        window.set_active_tab(0);
        window.set_control_name("a-edited".into());
        window.set_control_command_line("x-edited".into());

        handle_tab_changed(&window, &app, &update, Some(path.as_path()), 1);

        {
            let a = app.lock().unwrap();
            assert_eq!(a.configs[0].name, "a-edited");
            assert_eq!(a.configs[0].command_line, "x-edited");
        }

        let cfg = config::load(&path);
        assert_eq!(cfg.configs[0].name, "a-edited");
        assert_eq!(cfg.configs[0].command_line, "x-edited");
    }

    #[test]
    fn handle_control_add_focuses_the_name_field() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        window
            .window()
            .set_size(slint::WindowSize::Logical(slint::LogicalSize::new(
                1280.0, 800.0,
            )));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![], None)));

        window.set_active_tab(0);
        // Let the Control tab panel instantiate.
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
        handle_control_add(&window, &app, Some(path.as_path()));

        // The flag mirrors the real focus state, so it is not set by the
        // handler itself.
        assert!(!window.get_control_name_has_focus());
        // Run the queued change handlers (which apply the selection and move
        // the focus to the name field).
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
        assert!(window.get_control_name_has_focus());
    }

    #[test]
    fn handle_tab_changed_discards_the_pending_focus_request() {
        i_slint_backend_testing::init_no_event_loop();
        let window = MainWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let app = Arc::new(Mutex::new(test_app_with_configs(vec![], None)));
        let update = Arc::new(Mutex::new(UpdateState {
            enabled: false,
            period_days: 3,
            last_check_unix_ms: None,
            last_attempt_unix_ms: None,
            available: None,
        }));

        window.set_active_tab(0);
        // Let the Control tab panel instantiate.
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
        handle_control_add(&window, &app, Some(path.as_path()));
        handle_tab_changed(&window, &app, &update, Some(path.as_path()), 1);
        handle_tab_changed(&window, &app, &update, Some(path.as_path()), 0);
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));

        assert!(!window.get_control_name_has_focus());
    }
}
