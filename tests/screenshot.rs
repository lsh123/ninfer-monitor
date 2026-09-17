// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

slint::include_modules!();

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use i_slint_backend_testing::ElementHandle;
use ninfer_monitor::gpu::{self, DevicePoint, GpuHistory, GpuSample, GpuSnapshot, TempUnit};
use ninfer_monitor::kpi;
use ninfer_monitor::requests;
use ninfer_monitor::store::Store;
use png::{ColorType, Compression, Decoder, Encoder};
use slint::ComponentHandle;
use slint::ModelRc;
use slint::platform::PointerEventButton;

const SNAPSHOT_DIR: &str = "tests/snapshots";
const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;

const COLOR_LIVE: slint::Color = slint::Color::from_rgb_u8(0x4c, 0xaf, 0x50);
const COLOR_DISCONNECTED: slint::Color = slint::Color::from_rgb_u8(0xf4, 0x43, 0x36);

std::thread_local! {
    static PLATFORM_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn init_platform() {
    PLATFORM_READY.with(|ready| {
        if ready.get() {
            return;
        }
        i_slint_core::platform::set_platform(Box::new(
            i_slint_backend_testing::TestingBackend::new(
                i_slint_backend_testing::TestingBackendOptions {
                    mock_time: true,
                    threading: false,
                    renderer_name: Some(i_slint_core::SharedString::from("software")),
                },
            ),
        ))
        .expect("testing platform already initialized");
        ready.set(true);
    });
}

fn new_window() -> MainWindow {
    init_platform();
    let window = MainWindow::new().expect("create main window");
    window
        .window()
        .set_size(slint::WindowSize::Logical(slint::LogicalSize::new(
            WIDTH, HEIGHT,
        )));
    window
}

fn click(window: &MainWindow, id: &str) {
    let Some(elem) = ElementHandle::find_by_element_id(window, id).next() else {
        panic!("element not found: {id}");
    };
    elem.mock_single_click(PointerEventButton::Left);
}

fn capture(window: &MainWindow) -> (u32, u32, Vec<u8>) {
    let snapshot = window
        .window()
        .take_snapshot()
        .expect("take_snapshot failed (is the software renderer enabled?)");
    let width = snapshot.width();
    let height = snapshot.height();
    let rgba: Vec<u8> = snapshot
        .as_slice()
        .iter()
        .flat_map(|p| [p.r, p.g, p.b, p.a])
        .collect();
    (width, height, rgba)
}

/// The actual on-screen size of each chart panel, read from the widget out
/// properties. Forces a layout pass first so the out properties are
/// populated; falls back to the default render size when the layout has not
/// produced a non-zero size.
fn panel_sizes(window: &MainWindow) -> [(u32, u32); 4] {
    let _ = window.window().take_snapshot();
    fn one(w: f32, h: f32) -> (u32, u32) {
        if w <= 0.0 || h <= 0.0 {
            (
                ninfer_monitor::chart::CHART_WIDTH,
                ninfer_monitor::chart::CHART_HEIGHT,
            )
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

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut encoder = Encoder::new(&mut buf, width, height);
        encoder.set_color(ColorType::Rgba);
        encoder.set_compression(Compression::Balanced);
        let mut writer = encoder.write_header().expect("write png header");
        writer.write_image_data(rgba).expect("write png pixels");
    }
    buf
}

fn decode_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let data =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let mut reader = Decoder::new(Cursor::new(data))
        .read_info()
        .unwrap_or_else(|e| panic!("failed to decode {}: {e}", path.display()));
    let mut buf = vec![0u8; reader.output_buffer_size().expect("output buffer size")];
    let info = reader.next_frame(&mut buf).expect("read png frame");
    assert_eq!(
        info.color_type,
        ColorType::Rgba,
        "snapshot {} must be RGBA",
        path.display()
    );
    (info.width, info.height, buf)
}

fn actual_path(recorded: &Path) -> PathBuf {
    recorded
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".png"))
        .map(|stem| recorded.with_file_name(format!("{stem}.actual.png")))
        .expect("actual path")
}

fn write_actual(recorded: &Path, width: u32, height: u32, rgba: &[u8]) {
    let actual = actual_path(recorded);
    std::fs::write(&actual, encode_png(width, height, rgba)).expect("write actual image");
    eprintln!("actual image written to {}", actual.display());
}

fn check_snapshot(name: &str, width: u32, height: u32, rgba: &[u8]) {
    let path = Path::new(SNAPSHOT_DIR).join(format!("{name}.png"));
    let actual = actual_path(&path);
    if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
        std::fs::create_dir_all(SNAPSHOT_DIR).expect("create snapshot dir");
        std::fs::write(&path, encode_png(width, height, rgba)).expect("write snapshot");
        if actual.exists() {
            std::fs::remove_file(&actual).expect("remove stale actual image");
        }
        eprintln!("recorded {}", path.display());
        return;
    }
    if actual.exists() {
        panic!(
            "stale actual image {} left by a previous mismatch; delete it or re-record with: $env:UPDATE_SNAPSHOTS=1; cargo test --test screenshot",
            actual.display()
        );
    }
    if !path.exists() {
        panic!(
            "missing snapshot {}. Record it with: $env:UPDATE_SNAPSHOTS=1; cargo test --test screenshot",
            path.display()
        );
    }
    let (expected_width, expected_height, expected) = decode_png(&path);
    if (width, height) != (expected_width, expected_height) {
        write_actual(&path, width, height, rgba);
        panic!(
            "snapshot {} size mismatch: expected {expected_width}x{expected_height}, got {width}x{height}",
            path.display()
        );
    }
    let pixel_count = (width as usize) * (height as usize);
    let mut differing = 0;
    let mut first_differing = None;
    for i in 0..pixel_count {
        if rgba[i * 4..i * 4 + 4] != expected[i * 4..i * 4 + 4] {
            differing += 1;
            if first_differing.is_none() {
                first_differing = Some(i);
            }
        }
    }
    if differing > 0 {
        write_actual(&path, width, height, rgba);
        let (x, y) = first_differing
            .map(|i| (i % width as usize, i / width as usize))
            .expect("first differing pixel");
        panic!(
            "snapshot {} differs: {differing} of {pixel_count} pixels differ (first at ({x}, {y}))",
            path.display()
        );
    }
}

fn sample_store() -> Store {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/server.requests.jsonl");
    Store::load_jsonl(path).expect("read sample log")
}

/// The deterministic "current time" for the charts' right edge (M5): the
/// fixture's last event plus 20 s, so the snapshots show a short dashed
/// stale tail without depending on the wall clock.
fn now_ms(store: &Store) -> u64 {
    store.max_timestamp_ms().unwrap_or(0).saturating_add(20_000)
}

/// The deterministic "current time" for the synthetic GPU history (M5): 20 s
/// after its last sample (ts = 300_000).
const GPU_NOW_MS: u64 = 320_000;

fn apply_request_rows(window: &MainWindow, store: &Store) {
    let empty_ids = std::collections::HashSet::new();
    let rows_data = requests::table_rows(store, &empty_ids);
    let rows: Vec<RequestRow> = rows_data.iter().map(to_slint_row).collect();
    window.set_request_rows(ModelRc::from(Rc::new(slint::VecModel::from(rows.clone()))));
    window.set_table_content_height(slint_content_height(&rows) as f32);
}

fn set_live_state(window: &MainWindow) -> Rc<Store> {
    window.set_file_name("tests/fixtures/server.requests.jsonl".into());
    window.set_file_name_full("tests/fixtures/server.requests.jsonl".into());
    window.set_last_event("14:02:58.124".into());
    window.set_errors_text("Errors: 11".into());
    window.set_skipped_text("Skipped 0".into());
    window.set_connection_status("Live".into());
    window.set_status_color(COLOR_LIVE);
    let store = Rc::new(sample_store());
    let k = kpi::snapshot(&store);
    ninfer_monitor::apply_kpis!(window, &k);
    let info = ninfer_monitor::server_info::snapshot(&store);
    ninfer_monitor::apply_server_info!(window, &info);
    let images = ninfer_monitor::chart::render_all_sized(
        &store,
        300_000,
        &panel_sizes(window),
        now_ms(&store),
    );
    ninfer_monitor::apply_chart_images!(window, &images);
    apply_request_rows(window, &store);
    store
}

fn to_slint_row(r: &requests::TableRowData) -> RequestRow {
    RequestRow {
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
    }
}

/// Total scrollable table height for slint rows, mirroring
/// `requests::table_content_height`.
fn slint_content_height(rows: &[RequestRow]) -> u32 {
    let n = rows.len() as u32;
    let base = if n == 0 {
        0
    } else {
        n * requests::ROW_HEIGHT_PX + (n - 1) * requests::ROW_SPACING_PX
    };
    let details: u32 = rows
        .iter()
        .filter(|r| r.expanded)
        .map(|r| {
            requests::detail_block_height(
                !r.detail_materialization.as_str().is_empty(),
                !r.detail_error.as_str().is_empty(),
            )
        })
        .sum();
    base + details
}

/// A deterministic synthetic GPU history + snapshot (M4): one device with a
/// 31-point ramp across the 5 m window, so the charts are non-empty and stable.
fn synthetic_gpu() -> (GpuHistory, GpuSnapshot) {
    let mut history = GpuHistory::new();
    for i in 0..31 {
        let ts = i * 10_000u64;
        history.push(GpuSample {
            ts_ms: ts,
            devices: vec![DevicePoint {
                index: 0,
                utilization: Some(40.0 + (i as f64) * 1.4),
                memory_percent: Some(50.0 + (i as f64) * 0.9),
                temperature: Some(55.0 + (i as f64) * 0.5),
                memory_temperature: Some(60.0 + (i as f64) * 0.5),
            }],
        });
    }
    let snapshot = GpuSnapshot {
        available: true,
        devices: vec![gpu::GpuDevice {
            index: 0,
            name: Some("NVIDIA GeForce RTX 5090".to_owned()),
            utilization: Some(84),
            memory_used: Some(12 * 1024 * 1024 * 1024),
            memory_total: Some(32 * 1024 * 1024 * 1024),
            temperature: Some(70),
            memory_temperature: Some(75),
        }],
    };
    (history, snapshot)
}

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

/// Render the GPU rows at the actual panel sizes (M4). The GPU row is the same
/// height as a metric row, so its chart size is the throughput chart size.
fn render_gpu(window: &MainWindow, unit: TempUnit) {
    let (history, snapshot) = synthetic_gpu();
    let sizes = panel_sizes(window);
    let rows = gpu::build_rows(&snapshot, &history, unit, 300_000, sizes[0], GPU_NOW_MS);
    let slint_rows: Vec<GpuDevice> = rows.iter().map(to_slint_gpu).collect();
    window.set_gpu_devices(ModelRc::from(Rc::new(slint::VecModel::from(slint_rows))));
}

/// Seed the GPU model at the default chart size so the layout reflects the
/// extra row before `set_live_state` reads the panel sizes (M4).
fn seed_gpu_rows(window: &MainWindow, unit: TempUnit) {
    let (history, snapshot) = synthetic_gpu();
    let guess = (
        ninfer_monitor::chart::CHART_WIDTH,
        ninfer_monitor::chart::CHART_HEIGHT,
    );
    let rows = gpu::build_rows(&snapshot, &history, unit, 300_000, guess, GPU_NOW_MS);
    let slint_rows: Vec<GpuDevice> = rows.iter().map(to_slint_gpu).collect();
    window.set_gpu_devices(ModelRc::from(Rc::new(slint::VecModel::from(slint_rows))));
}

#[test]
fn screenshot_gpu() {
    // The GPU section (M4): one row per GPU with usage/memory and temperature
    // widgets, temperatures in °F.
    let window = new_window();
    seed_gpu_rows(&window, TempUnit::Fahrenheit);
    let _store = set_live_state(&window);
    render_gpu(&window, TempUnit::Fahrenheit);
    let (width, height, rgba) = capture(&window);
    check_snapshot("gpu", width, height, &rgba);
}

#[test]
fn screenshot_gpu_celsius() {
    // The GPU section with the temperature unit set to °C (M4): the widget
    // values, titles, and chart axes are all in Celsius.
    let window = new_window();
    seed_gpu_rows(&window, TempUnit::Celsius);
    let _store = set_live_state(&window);
    render_gpu(&window, TempUnit::Celsius);
    let (width, height, rgba) = capture(&window);
    check_snapshot("gpu-celsius", width, height, &rgba);
}

#[test]
fn screenshot_initial() {
    // The no-file startup state (PRD v0.2 §3.1/M1): the startup screen is
    // shown with the "no file" message and a disabled Start button.
    let window = new_window();
    window.set_startup_visible(true);
    window.set_startup_version(env!("CARGO_PKG_VERSION").into());
    window.set_startup_message(ninfer_monitor::startup::MSG_NO_FILE.into());
    window.set_startup_file("".into());
    window.set_startup_start_enabled(false);
    let (width, height, rgba) = capture(&window);
    check_snapshot("initial", width, height, &rgba);
}

#[test]
fn screenshot_startup_unreadable() {
    // The unreadable-file startup state (PRD v0.2 §3.1/M1): the startup screen
    // is shown with the "cannot be read" message, the file pre-filled, and a
    // disabled Start button.
    let window = new_window();
    window.set_startup_visible(true);
    window.set_startup_version(env!("CARGO_PKG_VERSION").into());
    window.set_startup_message(ninfer_monitor::startup::MSG_UNREADABLE.into());
    window.set_startup_file("C:\\NInfer\\logs\\server.requests.jsonl".into());
    window.set_startup_start_enabled(false);
    let (width, height, rgba) = capture(&window);
    check_snapshot("startup-unreadable", width, height, &rgba);
}

#[test]
fn screenshot_live() {
    let window = new_window();
    set_live_state(&window);
    let (width, height, rgba) = capture(&window);
    check_snapshot("live", width, height, &rgba);
}

#[test]
fn screenshot_disconnected() {
    // The real app keeps the last KPI values while disconnected (updates keep
    // flowing with the frozen store), so the snapshot shows the retained KPIs
    // with the disconnected status.
    let window = new_window();
    set_live_state(&window);
    window.set_connection_status("Disconnected".into());
    window.set_status_color(COLOR_DISCONNECTED);
    let (width, height, rgba) = capture(&window);
    check_snapshot("disconnected", width, height, &rgba);
}

#[test]
fn screenshot_cleared() {
    let window = new_window();
    set_live_state(&window);
    let empty = Store::new();
    let k = kpi::snapshot(&empty);
    ninfer_monitor::apply_kpis!(&window, &k);
    let info = ninfer_monitor::server_info::snapshot(&empty);
    ninfer_monitor::apply_server_info!(&window, &info);
    let images = ninfer_monitor::chart::render_all_sized(&empty, 300_000, &panel_sizes(&window), 0);
    ninfer_monitor::apply_chart_images!(&window, &images);
    window.set_request_rows(ModelRc::from(Rc::new(slint::VecModel::<RequestRow>::from(
        Vec::new(),
    ))));
    window.set_last_event("—".into());
    window.set_errors_text("Errors: 0".into());
    let (width, height, rgba) = capture(&window);
    check_snapshot("cleared", width, height, &rgba);
}

#[test]
fn screenshot_server_restart() {
    let window = new_window();
    let store = set_live_state(&window);
    let mut store = (*store).clone();
    // A synthetic second `server_start` 60 s before the last event: inside
    // the default 5 m window, so the restart marker is drawn on the charts.
    let ts = store.max_timestamp_ms().expect("last timestamp") - 60_000;
    let raw = format!(
        r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"serve-restart-test","timestamp_unix_ms":{ts},"event":"server_start","engine":{{"kv_cache":"bf16","kv_capacity":120000,"max_context":120000,"max_concurrency":4,"prefill_chunk":1024,"speculative_backend":"eagle","cuda_graph":false,"prefix_reuse":false}},"environment":{{"gpu_name":"NVIDIA GeForce RTX 4090","compute_capability_major":8,"compute_capability_minor":9,"cuda_driver_version":"12.4","cuda_runtime_version":"12.4","total_device_memory_bytes":25769803776}},"memory":{{"weights":{{"capacity_bytes":17179869184,"used_bytes":17179869184}},"sequence":{{"capacity_bytes":4294967296,"used_bytes":0}},"workspace":{{"capacity_bytes":1073741824,"used_bytes":0}},"available_after_weights_bytes":8589934592}},"server":{{"host":"127.0.0.1","port":9090,"public_model_id":"qwen3.8-27b-fp8","request_log_jsonl":"d:/NInfer/logs/server.requests.jsonl"}}}}"#
    );
    store.apply(&ninfer_monitor::parser::parse_line(raw.as_bytes()).unwrap());
    let k = kpi::snapshot(&store);
    ninfer_monitor::apply_kpis!(&window, &k);
    let info = ninfer_monitor::server_info::snapshot(&store);
    ninfer_monitor::apply_server_info!(&window, &info);
    let images = ninfer_monitor::chart::render_all_sized(
        &store,
        300_000,
        &panel_sizes(&window),
        now_ms(&store),
    );
    ninfer_monitor::apply_chart_images!(&window, &images);
    apply_request_rows(&window, &store);
    let (width, height, rgba) = capture(&window);
    check_snapshot("server-restart", width, height, &rgba);
}

#[test]
fn screenshot_chart_window_1h() {
    let window = new_window();
    set_live_state(&window);
    click(&window, "MainWindow::win-1h");
    // The real handler (main.rs) mirrors the clamped window into the UI;
    // this test does not wire it, so set the property directly.
    window.set_chart_window_ms(3_600_000);
    let store = sample_store();
    let images = ninfer_monitor::chart::render_all_sized(
        &store,
        3_600_000,
        &panel_sizes(&window),
        now_ms(&store),
    );
    ninfer_monitor::apply_chart_images!(&window, &images);
    let (width, height, rgba) = capture(&window);
    check_snapshot("chart-window-1h", width, height, &rgba);
}

#[test]
fn screenshot_settings() {
    let window = new_window();
    set_live_state(&window);
    window.set_settings_log_poll_ms(500);
    window.set_settings_gpu_poll_ms(5_000);
    window.set_settings_file("C:\\logs\\server.requests.jsonl".into());
    // Show the Windows-only NVIDIA game-popup link (PRD v0.2 §3.2) so the
    // snapshot is deterministic regardless of the host platform.
    window.set_nvidia_popup_visible(true);
    window.set_settings_visible(true);
    let (width, height, rgba) = capture(&window);
    check_snapshot("settings", width, height, &rgba);
}

#[test]
fn screenshot_about() {
    let window = new_window();
    set_live_state(&window);
    window.set_settings_log_poll_ms(500);
    window.set_settings_gpu_poll_ms(5_000);
    window.set_settings_visible(true);
    window.set_about_version(env!("CARGO_PKG_VERSION").into());
    window.set_about_visible(true);
    let (width, height, rgba) = capture(&window);
    check_snapshot("about", width, height, &rgba);
}

#[test]
fn screenshot_request_detail() {
    let window = new_window();
    let store = set_live_state(&window);
    let empty_ids = std::collections::HashSet::new();
    let first_id = requests::table_rows(&store, &empty_ids)
        .first()
        .map(|r| r.id.clone());
    let mut expanded_ids = std::collections::HashSet::new();
    if let Some(id) = first_id
        && let Ok(id) = id.parse::<u64>()
    {
        expanded_ids.insert(id);
    }
    let rows_data = requests::table_rows(&store, &expanded_ids);
    let rows: Vec<RequestRow> = rows_data.iter().map(to_slint_row).collect();
    window.set_request_rows(ModelRc::from(Rc::new(slint::VecModel::from(rows.clone()))));
    window.set_table_content_height(slint_content_height(&rows) as f32);
    let (width, height, rgba) = capture(&window);
    check_snapshot("request-detail", width, height, &rgba);
}

#[test]
fn screenshot_table_columns() {
    // The request table with the column widths changed from the defaults
    // (M7): the header sliders and the row cells follow the new widths (the
    // total stays 1136px, the available column width at the default 1280px
    // window).
    let window = new_window();
    set_live_state(&window);
    window.set_col_id(56.0);
    window.set_col_time(120.0);
    window.set_col_model(320.0);
    window.set_col_prompt(72.0);
    window.set_col_compl(72.0);
    window.set_col_think(72.0);
    window.set_col_ttft(72.0);
    window.set_col_total(72.0);
    window.set_col_reason(180.0);
    window.set_col_status(100.0);
    let (width, height, rgba) = capture(&window);
    check_snapshot("table-columns", width, height, &rgba);
}

#[test]
fn screenshot_update() {
    // The update-available state (M8 §5.3): the background check found a
    // newer release, so the update button is shown in the toolbar, left of
    // the settings button.
    let window = new_window();
    set_live_state(&window);
    window.set_update_available(true);
    window.set_update_version("0.3.0".into());
    window.set_update_release_url(
        "https://github.com/lsh123/ninfer-monitor/releases/tag/v0.3.0".into(),
    );
    let (width, height, rgba) = capture(&window);
    check_snapshot("update", width, height, &rgba);
}

#[test]
fn screenshot_update_dialog() {
    // The "New version available" dialog (M8 §5.3) over the live dashboard:
    // title, the version question, the release notes link, and OK/Cancel.
    let window = new_window();
    set_live_state(&window);
    window.set_update_available(true);
    window.set_update_version("0.3.0".into());
    window.set_update_release_url(
        "https://github.com/lsh123/ninfer-monitor/releases/tag/v0.3.0".into(),
    );
    window.set_update_visible(true);
    let (width, height, rgba) = capture(&window);
    check_snapshot("update-dialog", width, height, &rgba);
}
