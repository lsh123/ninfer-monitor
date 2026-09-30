// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

slint::include_modules!();

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use i_slint_backend_testing::ElementHandle;
use ninfer_monitor::columns::{Column, ColumnWidths};
use ninfer_monitor::requests;
use slint::ComponentHandle;
use slint::platform::PointerEventButton;

/// The values captured by the settings-save callback (FR-7.1, M8, v0.3 M2).
#[derive(Clone)]
struct SavedSettings {
    log_poll_ms: i32,
    gpu_poll_ms: i32,
    max_requests: i32,
    install_folder: String,
    log_file_folder: String,
    temp_unit: String,
    update_check_enabled: bool,
    update_check_days: i32,
}

fn make_row(id: &str, status: &str) -> RequestRow {
    RequestRow {
        id: id.into(),
        time: "12:00:00.000".into(),
        model: "qwen3.8-27b-nvfp4".into(),
        prompt: "100".into(),
        compl: "50".into(),
        thinking: "5".into(),
        ttft: "500 ms".into(),
        total: "2.5 s".into(),
        reason: "stop_token".into(),
        status: status.into(),
        color: slint::Color::from_rgb_u8(0xd0, 0xd0, 0xd0),
        expanded: false,
        detail_sampling: "temperature 0.100".into(),
        detail_engine: "decode rounds 47".into(),
        detail_speculative: "backend mtp".into(),
        detail_tool_call: "structured 1".into(),
        detail_prefix: "root".into(),
        detail_materialization: "".into(),
        detail_error: "".into(),
    }
}

fn first_row(window: &MainWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(window, "RequestTable::row-el")
        .next()
        .expect("first table row")
}

fn first_row_ta(window: &MainWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(window, "TableRow::ta")
        .next()
        .expect("first row touch area")
}

fn set_rows(window: &MainWindow, rows: Vec<RequestRow>) {
    window.set_request_rows(slint::ModelRc::from(Rc::new(slint::VecModel::from(rows))));
}

/// Total scrollable table height for slint rows, mirroring
/// `requests::table_content_height` (the expansion state is owned by the
/// caller, as it is by `ChartState` in the app).
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

/// Set the rows and wire a `row-clicked` handler that toggles the clicked
/// row's `expanded` flag and rebuilds the model, standing in for the
/// `ChartState` toggle in the app.
fn set_expandable_rows(window: &MainWindow, rows: Vec<RequestRow>) {
    let shared: Arc<Mutex<Vec<RequestRow>>> = Arc::new(Mutex::new(rows));
    {
        let rs = shared.lock().unwrap();
        window.set_request_rows(slint::ModelRc::from(Rc::new(slint::VecModel::from(
            rs.clone(),
        ))));
        window.set_table_content_height(slint_content_height(&rs) as f32);
    }
    window.on_row_clicked({
        let weak = window.as_weak();
        let shared = shared.clone();
        move |id: slint::SharedString| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let mut rs = shared.lock().unwrap();
            if let Some(r) = rs.iter_mut().find(|r| r.id.as_str() == id.as_str()) {
                r.expanded = !r.expanded;
            }
            w.set_request_rows(slint::ModelRc::from(Rc::new(slint::VecModel::from(
                rs.clone(),
            ))));
            w.set_table_content_height(slint_content_height(&rs) as f32);
        }
    });
}

const MAIN_WINDOW_IDS: &[&str] = &[
    "tab-bar",
    "tab-control",
    "tab-monitor",
    "tab-settings",
    "btn-about",
    "header-bar",
    "widget-throughput",
    "widget-latency",
    "widget-cache",
    "widget-scheduler",
    "request-table",
    "server-info",
    "status-bar",
    "win-1m",
    "win-5m",
    "win-15m",
    "win-30m",
    "win-1h",
];

fn new_window() -> MainWindow {
    i_slint_backend_testing::init_no_event_loop();
    let window = MainWindow::new().unwrap();
    window
        .window()
        .set_size(slint::WindowSize::Logical(slint::LogicalSize::new(
            1280.0, 800.0,
        )));
    window
}

fn wire_stub_handlers(window: &MainWindow) -> Arc<Mutex<String>> {
    let last_action: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    window.on_window_changed({
        let last_action = last_action.clone();
        move |ms| {
            *last_action.lock().unwrap() = format!("window-changed:{ms}");
        }
    });
    window.on_tab_changed({
        let last_action = last_action.clone();
        move |which| {
            *last_action.lock().unwrap() = format!("tab-changed:{which}");
        }
    });
    last_action
}

fn last_action(h: &Arc<Mutex<String>>) -> String {
    h.lock().unwrap().clone()
}

fn send_key(window: &MainWindow, ch: char) {
    let adapter = i_slint_core::window::WindowInner::from_pub(window.window()).window_adapter();
    let text = i_slint_core::SharedString::from(ch);
    i_slint_backend_testing::testing_backend::send_keyboard_key_text(&text, true, &adapter);
}

fn click(window: &MainWindow, id: &str) {
    let Some(elem) = ElementHandle::find_by_element_id(window, id).next() else {
        panic!("element not found: {id}");
    };
    elem.mock_single_click(PointerEventButton::Left);
}

/// Clicks at an absolute window position (for points not covered by a
/// specific element's center, e.g. a dialog backdrop).
fn click_at(window: &MainWindow, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
    window
        .window()
        .dispatch_event(i_slint_core::platform::WindowEvent::PointerMoved { position });
    window
        .window()
        .dispatch_event(i_slint_core::platform::WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(50));
    window
        .window()
        .dispatch_event(i_slint_core::platform::WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
}

#[test]
fn window_is_1280x800() {
    let window = new_window();
    let size = window.window().size();
    assert_eq!(size.width, 1280);
    assert_eq!(size.height, 800);
}

#[test]
fn all_sections_present() {
    let window = new_window();
    for id in MAIN_WINDOW_IDS {
        let qualified = format!("MainWindow::{id}");
        let count = ElementHandle::find_by_element_id(&window, &qualified).count();
        assert!(count >= 1, "missing element: {qualified}");
    }
}

#[test]
fn no_layout_overflow_at_1280x800() {
    let window = new_window();
    for id in MAIN_WINDOW_IDS {
        let qualified = format!("MainWindow::{id}");
        let Some(elem) = ElementHandle::find_by_element_id(&window, &qualified).next() else {
            panic!("element not found: {qualified}");
        };
        let pos = elem.absolute_position();
        let size = elem.size();
        assert!(
            pos.x >= 0.0 && pos.y >= 0.0,
            "{qualified} has negative position ({}, {})",
            pos.x,
            pos.y
        );
        assert!(
            pos.x + size.width <= 1281.0 && pos.y + size.height <= 801.0,
            "{qualified} overflows 1280x800: pos=({}, {}) size=({}, {})",
            pos.x,
            pos.y,
            size.width,
            size.height
        );
    }
}

#[test]
fn scheduler_metrics_centered_under_labels() {
    let window = new_window();
    window.set_kpi_active_value("0".into());
    window.set_kpi_waiting_value("0".into());
    let _ = window.window().take_snapshot();

    let texts: Vec<(String, f32)> = i_slint_backend_testing::ElementQuery::from_root(&window)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter_map(|e| {
            let label = e.accessible_label()?;
            let p = e.absolute_position();
            let s = e.size();
            Some((label.to_string(), p.x + s.width / 2.0))
        })
        .collect();

    fn label_cx(texts: &[(String, f32)], s: &str) -> f32 {
        texts
            .iter()
            .find(|t| t.0 == s)
            .map(|t| t.1)
            .unwrap_or_else(|| panic!("label {s:?} not found"))
    }

    fn nearest_value_cx(texts: &[(String, f32)], value: &str, target_cx: f32) -> f32 {
        texts
            .iter()
            .filter(|t| t.0 == value)
            .min_by(|a, b| {
                (a.1 - target_cx)
                    .abs()
                    .partial_cmp(&(b.1 - target_cx).abs())
                    .unwrap()
            })
            .map(|t| t.1)
            .unwrap_or_else(|| panic!("value {value:?} not found"))
    }

    // The Scheduler shows no trend arrow, so each value must sit exactly
    // under its label (both centered in the 120px side column).
    let active_label = label_cx(&texts, "Active");
    let active_value = nearest_value_cx(&texts, "0", active_label);
    let waiting_label = label_cx(&texts, "Waiting");
    let waiting_value = nearest_value_cx(&texts, "0", waiting_label);

    assert!(
        (active_label - active_value).abs() < 1.0,
        "Active: label cx {active_label} != value cx {active_value}"
    );
    assert!(
        (waiting_label - waiting_value).abs() < 1.0,
        "Waiting: label cx {waiting_label} != value cx {waiting_value}"
    );
}

#[test]
fn metric_widgets_are_equal_size() {
    let window = new_window();
    let widths = [
        window.get_widget_throughput_w(),
        window.get_widget_latency_w(),
        window.get_widget_cache_w(),
        window.get_widget_scheduler_w(),
    ];
    let heights = [
        window.get_widget_throughput_h(),
        window.get_widget_latency_h(),
        window.get_widget_cache_h(),
        window.get_widget_scheduler_h(),
    ];
    for i in 1..4 {
        assert!(
            (widths[i] - widths[0]).abs() < 1.0,
            "widget {i} chart width {} != widget 0 chart width {}",
            widths[i],
            widths[0]
        );
        assert!(
            (heights[i] - heights[0]).abs() < 1.0,
            "widget {i} chart height {} != widget 0 chart height {}",
            heights[i],
            heights[0]
        );
    }

    window.set_kpi_prefill_value("2461.7".into());
    window.set_kpi_decode_value("123.9".into());
    window.set_kpi_ttft_value("421.9".into());
    window.set_kpi_decode_lat_value("8.0".into());
    window.set_kpi_cache_value("99.7".into());
    window.set_kpi_spec_value("77.3".into());
    window.set_kpi_active_value("3".into());
    window.set_kpi_waiting_value("0".into());
    let _ = window.window().take_snapshot();
    let live_widths = [
        window.get_widget_throughput_w(),
        window.get_widget_latency_w(),
        window.get_widget_cache_w(),
        window.get_widget_scheduler_w(),
    ];
    let live_heights = [
        window.get_widget_throughput_h(),
        window.get_widget_latency_h(),
        window.get_widget_cache_h(),
        window.get_widget_scheduler_h(),
    ];
    for i in 1..4 {
        assert!(
            (live_widths[i] - live_widths[0]).abs() < 1.0,
            "live: widget {i} chart width {} != widget 0 chart width {}",
            live_widths[i],
            live_widths[0]
        );
        assert!(
            (live_heights[i] - live_heights[0]).abs() < 1.0,
            "live: widget {i} chart height {} != widget 0 chart height {}",
            live_heights[i],
            live_heights[0]
        );
    }
}

#[test]
fn latency_widget_shows_units_next_to_values() {
    let window = new_window();
    window.set_kpi_ttft_value("446".into());
    window.set_kpi_ttft_unit("ms".into());
    window.set_kpi_ttft_trend(1);
    window.set_kpi_decode_lat_value("7.3".into());
    window.set_kpi_decode_lat_unit("ms".into());
    let _ = window.window().take_snapshot();

    let widget = ElementHandle::find_by_element_id(&window, "MainWindow::widget-latency")
        .next()
        .unwrap();
    let pos = widget.absolute_position();
    let size = widget.size();

    let units = i_slint_backend_testing::ElementQuery::from_root(&window)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter(|e| e.accessible_label().is_some_and(|l| l.as_str() == "ms"))
        .filter(|e| {
            let p = e.absolute_position();
            p.x >= pos.x && p.x < pos.x + size.width && p.y >= pos.y && p.y < pos.y + size.height
        })
        .count();
    assert_eq!(units, 2, "both latency values carry a unit label");
}

#[test]
fn request_table_height_is_stable_regardless_of_content() {
    let window = new_window();
    let table = || {
        ElementHandle::find_by_element_id(&window, "MainWindow::request-table")
            .next()
            .unwrap()
            .size()
    };
    // The table height is driven by `table-frac`, not by its content: it must
    // be identical with an empty table and a full one (rows scroll inside).
    let empty_height = table().height;
    assert!(empty_height > 0.0, "table has a layout height");

    let rows: Vec<RequestRow> = (0..50).map(|i| make_row(&i.to_string(), "done")).collect();
    set_expandable_rows(&window, rows);
    click(&window, "TableRow::ta");
    let _ = window.window().take_snapshot();
    assert_eq!(
        table().height,
        empty_height,
        "table height must not grow with its content"
    );
    let chart = ElementHandle::find_by_element_id(&window, "MainWindow::widget-throughput")
        .next()
        .unwrap()
        .size();
    assert!(chart.height > 40.0, "chart widgets collapsed: {chart:?}");
}

#[test]
fn window_selector_button_fires_callback() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    // The window selector is on the Monitor tab; activate it so the button is
    // not covered by the default (Control) tab overlay.
    window.set_active_tab(1);
    click(&window, "MainWindow::win-1h");
    assert_eq!(last_action(&action), "window-changed:3600000");
}

#[test]
fn window_selector_30m_button_fires_callback() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    window.set_active_tab(1);
    click(&window, "MainWindow::win-30m");
    assert_eq!(last_action(&action), "window-changed:1800000");
}

#[test]
fn control_tab_is_default() {
    let window = new_window();
    assert_eq!(window.get_active_tab(), 0, "Control is the default tab");
}

#[test]
fn tab_button_fires_callback() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    click(&window, "MainWindow::tab-settings");
    assert_eq!(last_action(&action), "tab-changed:2");
    click(&window, "MainWindow::tab-control");
    assert_eq!(last_action(&action), "tab-changed:0");
    click(&window, "MainWindow::tab-monitor");
    assert_eq!(last_action(&action), "tab-changed:1");
}

#[test]
fn tab_switching_shows_the_active_tab_content() {
    let window = new_window();
    let count = |id: &str| ElementHandle::find_by_element_id(&window, id).count();
    // The Control tab is active by default: the Monitor tab content (always
    // present) and the Control overlay are shown, the Settings overlay is not.
    assert_eq!(count("MainWindow::monitor-tab"), 1);
    assert_eq!(count("MainWindow::control-tab"), 1);
    assert_eq!(count("MainWindow::settings-panel"), 0);
    window.set_active_tab(1);
    assert_eq!(count("MainWindow::control-tab"), 0);
    assert_eq!(count("MainWindow::settings-panel"), 0);
    window.set_active_tab(2);
    assert_eq!(count("MainWindow::control-tab"), 0);
    assert_eq!(count("MainWindow::settings-panel"), 1);
}

#[test]
fn settings_panel_fills_tab_content() {
    let window = new_window();
    window.set_active_tab(2);
    let panel = ElementHandle::find_by_element_id(&window, "MainWindow::settings-panel")
        .next()
        .unwrap();
    let content = ElementHandle::find_by_element_id(&window, "MainWindow::tab-content")
        .next()
        .unwrap();
    let p = panel.size();
    let c = content.size();
    assert!(
        (p.width - c.width).abs() < 1.0 && (p.height - c.height).abs() < 1.0,
        "settings panel fills the tab content area: panel={p:?} content={c:?}"
    );
}

#[test]
fn settings_temp_unit_group_keeps_its_height_on_resize() {
    let window = new_window();
    window.set_active_tab(2);
    let group_h = |window: &MainWindow| {
        let label = ElementHandle::find_by_element_id(window, "SettingsPanel::temp-label")
            .next()
            .expect("temp label");
        let btn = ElementHandle::find_by_element_id(window, "SettingsPanel::temp-f")
            .next()
            .expect("temp F button");
        (btn.absolute_position().y + btn.size().height) - label.absolute_position().y
    };
    let _ = window.window().take_snapshot();
    let base = group_h(&window);
    // The temperature-unit group (header + F/C buttons) has fixed content:
    // its height must not change when the window is resized, so the buttons
    // stay right under the header (the extra vertical space goes to the
    // stretch spacer above the footer).
    for h in [1000.0, 650.0] {
        window
            .window()
            .set_size(slint::WindowSize::Logical(slint::LogicalSize::new(
                1280.0, h,
            )));
        let _ = window.window().take_snapshot();
        assert!(
            (group_h(&window) - base).abs() < 1.0,
            "temp unit group height changed on resize to {h}: {base} -> {}",
            group_h(&window)
        );
    }
}

#[test]
fn nvidia_link_hidden_by_default() {
    let window = new_window();
    window.set_active_tab(2);
    let count = ElementHandle::find_by_element_id(&window, "SettingsPanel::nvidia-link").count();
    assert_eq!(
        count, 0,
        "nvidia link hidden when nvidia-popup-visible is false"
    );
    window.set_nvidia_popup_visible(true);
    let count = ElementHandle::find_by_element_id(&window, "SettingsPanel::nvidia-link").count();
    assert_eq!(count, 1, "nvidia link visible when nvidia-popup-visible");
}

#[test]
fn nvidia_link_fires_callback() {
    let window = new_window();
    window.set_active_tab(2);
    window.set_nvidia_popup_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_nvidia_popup({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::nvidia-ta");
    assert!(*fired.lock().unwrap(), "nvidia-popup callback not fired");
}

#[test]
fn settings_panel_save_fires_callback_with_current_values() {
    let window = new_window();
    window.set_active_tab(2);
    window.set_settings_log_poll_ms(1500);
    window.set_settings_gpu_poll_ms(3000);
    window.set_settings_max_requests(250);
    window.set_settings_install_folder("C:\\ninfer".into());
    window.set_settings_log_file_folder("C:\\logs".into());
    window.set_settings_update_check_enabled(false);
    window.set_settings_update_check_days(7);
    let last: Arc<Mutex<Option<SavedSettings>>> = Arc::new(Mutex::new(None));
    window.on_settings_save({
        let last = last.clone();
        move |log_poll_ms: i32,
              gpu_poll_ms: i32,
              max_requests: i32,
              install_folder: slint::SharedString,
              log_file_folder: slint::SharedString,
              temp_unit: slint::SharedString,
              update_check_enabled: bool,
              update_check_days: i32| {
            *last.lock().unwrap() = Some(SavedSettings {
                log_poll_ms,
                gpu_poll_ms,
                max_requests,
                install_folder: install_folder.to_string(),
                log_file_folder: log_file_folder.to_string(),
                temp_unit: temp_unit.to_string(),
                update_check_enabled,
                update_check_days,
            });
        }
    });
    click(&window, "SettingsPanel::btn-save");
    let SavedSettings {
        log_poll_ms,
        gpu_poll_ms,
        max_requests,
        install_folder,
        log_file_folder,
        temp_unit,
        update_check_enabled,
        update_check_days,
    } = last.lock().unwrap().clone().expect("save not fired");
    assert_eq!(log_poll_ms, 1500);
    assert_eq!(gpu_poll_ms, 3000);
    assert_eq!(max_requests, 250);
    assert_eq!(install_folder, "C:\\ninfer");
    assert_eq!(log_file_folder, "C:\\logs");
    assert_eq!(temp_unit, "F");
    assert!(!update_check_enabled);
    assert_eq!(update_check_days, 7);
}

#[test]
fn settings_folder_inputs_roundtrip() {
    let window = new_window();
    window.set_active_tab(2);
    assert_eq!(window.get_settings_install_folder(), "");
    assert_eq!(window.get_settings_log_file_folder(), "");
    window.set_settings_install_folder("C:\\ninfer".into());
    window.set_settings_log_file_folder("C:\\logs".into());
    assert_eq!(window.get_settings_install_folder(), "C:\\ninfer");
    assert_eq!(window.get_settings_log_file_folder(), "C:\\logs");
}

#[test]
fn settings_open_folder_button_fires_callback() {
    let window = new_window();
    window.set_active_tab(2);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings_open_folder({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::btn-open-folder");
    assert!(
        *fired.lock().unwrap(),
        "open-folder button must fire the settings-open-folder callback"
    );
}

#[test]
fn settings_open_log_folder_button_fires_callback() {
    let window = new_window();
    window.set_active_tab(2);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings_open_log_folder({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::btn-open-log-folder");
    assert!(
        *fired.lock().unwrap(),
        "open-log-folder button must fire the settings-open-log-folder callback"
    );
}

#[test]
fn settings_check_updates_button_fires_callback() {
    let window = new_window();
    window.set_active_tab(2);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings_check_updates({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::btn-check-updates");
    assert!(
        *fired.lock().unwrap(),
        "check-updates button must fire the settings-check-updates callback"
    );
}

#[test]
fn settings_import_configs_button_fires_callback() {
    let window = new_window();
    window.set_active_tab(2);
    window.set_settings_import_enabled(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings_import_configs({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::btn-import-configs");
    assert!(
        *fired.lock().unwrap(),
        "import-configs button must fire the settings-import-configs callback"
    );
}

#[test]
fn settings_import_configs_button_inert_when_disabled() {
    let window = new_window();
    window.set_active_tab(2);
    assert!(!window.get_settings_import_enabled());
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings_import_configs({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::btn-import-configs");
    assert!(
        !*fired.lock().unwrap(),
        "a disabled import-configs button must not fire the callback"
    );
}

#[test]
fn settings_import_status_text_is_hidden_when_empty() {
    let window = new_window();
    window.set_active_tab(2);
    let _ = window.window().take_snapshot();
    let count = |id: &str| ElementHandle::find_by_element_id(&window, id).count();
    assert_eq!(
        count("SettingsPanel::import-status-text"),
        0,
        "the import status text must be hidden while the status is empty"
    );
    window.set_settings_import_status("Imported 3 configurations.".into());
    let _ = window.window().take_snapshot();
    assert_eq!(
        count("SettingsPanel::import-status-text"),
        1,
        "the import status text must be visible while the status is set"
    );
}

#[test]
fn settings_check_updates_button_is_below_the_update_check_group() {
    let window = new_window();
    window.set_active_tab(2);
    let _ = window.window().take_snapshot();
    let group = ElementHandle::find_by_element_id(&window, "SettingsPanel::update-check-group")
        .next()
        .expect("update-check-group not found");
    let btn = ElementHandle::find_by_element_id(&window, "SettingsPanel::btn-check-updates")
        .next()
        .expect("btn-check-updates not found");
    let group_bottom = group.absolute_position().y + group.size().height;
    let btn_top = btn.absolute_position().y;
    assert!(
        btn_top >= group_bottom,
        "the button is below the update-check group: btn_top={btn_top}, group_bottom={group_bottom}"
    );
}

#[test]
fn settings_slider_click_updates_log_poll_ms() {
    let window = new_window();
    window.set_active_tab(2);
    let initial = window.get_settings_log_poll_ms();
    click(&window, "SettingsPanel::log-poll-slider");
    let updated = window.get_settings_log_poll_ms();
    assert_ne!(
        updated, initial,
        "clicking the slider must change the log file poll interval"
    );
    assert!(
        (100..=5_000).contains(&updated),
        "log file poll interval stays in range: {updated}"
    );
}

#[test]
fn settings_slider_click_updates_gpu_poll_ms() {
    let window = new_window();
    window.set_active_tab(2);
    let initial = window.get_settings_gpu_poll_ms();
    click(&window, "SettingsPanel::gpu-poll-slider");
    let updated = window.get_settings_gpu_poll_ms();
    assert_ne!(
        updated, initial,
        "clicking the slider must change the GPU poll interval"
    );
    assert!(
        (1_000..=10_000).contains(&updated),
        "GPU poll interval stays in range: {updated}"
    );
}

#[test]
fn settings_log_poll_label_formats_value() {
    let window = new_window();
    window.set_active_tab(2);
    let label = || {
        ElementHandle::find_by_element_id(&window, "SettingsPanel::log-poll-label")
            .next()
            .and_then(|e| e.accessible_label())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    window.set_settings_log_poll_ms(500);
    assert_eq!(label(), "Log file poll interval: 500 ms");
    window.set_settings_log_poll_ms(1500);
    assert_eq!(label(), "Log file poll interval: 1.5 s");
    window.set_settings_log_poll_ms(5_000);
    assert_eq!(label(), "Log file poll interval: 5 s");
}

#[test]
fn settings_gpu_poll_label_formats_value() {
    let window = new_window();
    window.set_active_tab(2);
    let label = || {
        ElementHandle::find_by_element_id(&window, "SettingsPanel::gpu-poll-label")
            .next()
            .and_then(|e| e.accessible_label())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    window.set_settings_gpu_poll_ms(1_000);
    assert_eq!(label(), "GPU poll interval: 1 s");
    window.set_settings_gpu_poll_ms(5_000);
    assert_eq!(label(), "GPU poll interval: 5 s");
    window.set_settings_gpu_poll_ms(10_000);
    assert_eq!(label(), "GPU poll interval: 10 s");
}

#[test]
fn settings_slider_click_updates_max_requests() {
    let window = new_window();
    window.set_active_tab(2);
    let initial = window.get_settings_max_requests();
    click(&window, "SettingsPanel::max-slider");
    let updated = window.get_settings_max_requests();
    assert_ne!(
        updated, initial,
        "clicking the slider must change the max requests"
    );
    assert!(
        (10..=1_000).contains(&updated),
        "max requests stays in range: {updated}"
    );
}

#[test]
fn settings_max_requests_label_formats_value() {
    let window = new_window();
    window.set_active_tab(2);
    let label = || {
        ElementHandle::find_by_element_id(&window, "SettingsPanel::max-label")
            .next()
            .and_then(|e| e.accessible_label())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    window.set_settings_max_requests(1000);
    assert_eq!(label(), "Max requests: 1000");
    window.set_settings_max_requests(250);
    assert_eq!(label(), "Max requests: 250");
    window.set_settings_max_requests(10);
    assert_eq!(label(), "Max requests: 10");
}

#[test]
fn settings_panel_discard_fires_discard() {
    let window = new_window();
    window.set_active_tab(2);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings_discard({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::btn-discard");
    assert!(
        *fired.lock().unwrap(),
        "Discard must fire the settings-discard callback"
    );
}

#[test]
fn settings_escape_discards() {
    let window = new_window();
    window.set_active_tab(2);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings_discard({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::log-poll-label");
    send_key(&window, '\u{1b}');
    assert!(
        *fired.lock().unwrap(),
        "Escape must fire the settings-discard callback"
    );
}

#[test]
fn about_panel_hidden_by_default() {
    let window = new_window();
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::about-panel").count();
    assert_eq!(count, 0, "about panel hidden by default");
    window.set_about_visible(true);
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::about-panel").count();
    assert_eq!(count, 1, "about panel visible when about-visible");
}

#[test]
fn about_panel_hugs_content_vertically() {
    let window = new_window();
    window.set_about_visible(true);
    let panel = ElementHandle::find_by_element_id(&window, "MainWindow::about-panel")
        .next()
        .unwrap();
    let size = panel.size();
    assert_eq!(size.width, 380.0, "about panel keeps its 380px width");
    assert!(
        (150.0..480.0).contains(&size.height),
        "about panel height is content-based: {size:?}"
    );
}

#[test]
fn about_panel_shows_app_icon_to_the_left_of_app_info() {
    let window = new_window();
    window.set_about_visible(true);
    let icon = ElementHandle::find_by_element_id(&window, "AboutPanel::app-icon")
        .next()
        .expect("app icon");
    let app_info = ElementHandle::find_by_element_id(&window, "AboutPanel::app-info")
        .next()
        .expect("app info");
    let icon_pos = icon.absolute_position();
    let icon_size = icon.size();
    let app_info_pos = app_info.absolute_position();
    let app_info_size = app_info.size();
    assert_eq!(icon_size.width, 48.0, "app icon keeps its 48px width");
    assert_eq!(icon_size.height, 48.0, "app icon keeps its 48px height");
    assert!(
        icon_pos.x + icon_size.width < app_info_pos.x,
        "app icon must be to the left of the app name / version / copyright block"
    );
    let icon_center_y = icon_pos.y + icon_size.height / 2.0;
    let app_info_center_y = app_info_pos.y + app_info_size.height / 2.0;
    assert!(
        (icon_center_y - app_info_center_y).abs() < 1.0,
        "app icon and app info must be vertically centered relative to each other: \
         icon_center_y={:.1} app_info_center_y={:.1}",
        icon_center_y,
        app_info_center_y
    );
}

#[test]
fn about_button_opens_about_dialog() {
    let window = new_window();
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_about({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "MainWindow::btn-about");
    assert!(
        *fired.lock().unwrap(),
        "the about button must fire the about callback"
    );
}

#[test]
fn about_ok_closes_dialog() {
    let window = new_window();
    window.set_about_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_about_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "AboutPanel::btn-ok");
    assert!(*fired.lock().unwrap(), "OK must close the about dialog");
}

#[test]
fn about_escape_closes_dialog() {
    let window = new_window();
    window.set_about_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_about_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "AboutPanel::about-texts");
    send_key(&window, '\u{1b}');
    assert!(*fired.lock().unwrap(), "Escape must close the about dialog");
}

#[test]
fn about_escape_closes_after_opening_from_tab_bar() {
    let window = new_window();
    window.on_about({
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                w.set_about_visible(true);
            }
        }
    });
    window.on_about_close({
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                w.set_about_visible(false);
            }
        }
    });
    click(&window, "MainWindow::btn-about");
    assert_eq!(
        ElementHandle::find_by_element_id(&window, "MainWindow::about-panel").count(),
        1,
        "the about dialog must be open after clicking the tab-bar button"
    );
    click(&window, "AboutPanel::about-texts");
    send_key(&window, '\u{1b}');
    assert_eq!(
        ElementHandle::find_by_element_id(&window, "MainWindow::about-panel").count(),
        0,
        "Escape must close the about dialog"
    );
}

#[test]
fn settings_escape_closes_about_not_settings_when_about_open() {
    let window = new_window();
    window.set_active_tab(2);
    click(&window, "SettingsPanel::log-poll-label");
    window.set_about_visible(true);
    let about_fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let settings_fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_about_close({
        let about_fired = about_fired.clone();
        move || {
            *about_fired.lock().unwrap() = true;
        }
    });
    window.on_settings_discard({
        let settings_fired = settings_fired.clone();
        move || {
            *settings_fired.lock().unwrap() = true;
        }
    });
    send_key(&window, '\u{1b}');
    assert!(
        *about_fired.lock().unwrap(),
        "Escape must close the about dialog"
    );
    assert!(
        !*settings_fired.lock().unwrap(),
        "Escape must not discard settings while about is open"
    );
}

#[test]
fn about_version_is_rendered() {
    let window = new_window();
    window.set_about_version("0.1.0".into());
    window.set_about_visible(true);
    let _ = window.window().take_snapshot();
    let texts: Vec<String> = i_slint_backend_testing::ElementQuery::from_root(&window)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter_map(|e| e.accessible_label().map(|s| s.to_string()))
        .collect();
    assert!(
        texts.iter().any(|t| t == "NInferMonitor v0.1.0"),
        "about dialog must show the build version, got: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t == "Copyright (C) 2026 Aleksey Sanin"),
        "about dialog must show the copyright, got: {texts:?}"
    );
}

#[test]
fn startup_screen_hidden_by_default() {
    let window = new_window();
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::startup-panel").count();
    assert_eq!(count, 0, "startup screen hidden by default");
    window.set_startup_visible(true);
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::startup-panel").count();
    assert_eq!(count, 1, "startup screen visible when startup-visible");
}

#[test]
fn startup_close_button_fires_callback() {
    let window = new_window();
    window.set_startup_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_startup_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "MainWindow::startup-btn-close");
    assert!(
        *fired.lock().unwrap(),
        "Close must fire the startup-close callback"
    );
}

#[test]
fn startup_escape_closes() {
    let window = new_window();
    window.set_startup_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_startup_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "MainWindow::startup-spacer");
    send_key(&window, '\u{1b}');
    assert!(
        *fired.lock().unwrap(),
        "Escape must fire the startup-close callback"
    );
}

#[test]
fn startup_open_folder_button_fires_callback() {
    let window = new_window();
    window.set_startup_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_startup_open_folder({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "MainWindow::startup-btn-open");
    assert!(
        *fired.lock().unwrap(),
        "Open Folder must fire the startup-open-folder callback"
    );
}

#[test]
fn startup_start_button_fires_callback_when_enabled() {
    let window = new_window();
    window.set_startup_visible(true);
    window.set_startup_start_enabled(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_startup_start({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "MainWindow::startup-btn-start");
    assert!(
        *fired.lock().unwrap(),
        "Start must fire the startup-start callback when enabled"
    );
}

#[test]
fn startup_start_button_inert_when_disabled() {
    let window = new_window();
    window.set_startup_visible(true);
    window.set_startup_start_enabled(false);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_startup_start({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "MainWindow::startup-btn-start");
    assert!(
        !*fired.lock().unwrap(),
        "Start must not fire while disabled"
    );
}

#[test]
fn startup_welcome_text_shows_version() {
    let window = new_window();
    window.set_startup_visible(true);
    window.set_startup_version("0.2.0".into());
    let _ = window.window().take_snapshot();
    let texts: Vec<String> = i_slint_backend_testing::ElementQuery::from_root(&window)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter_map(|e| e.accessible_label().map(|s| s.to_string()))
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t == "Welcome to NInferMonitor (v0.2.0)!"),
        "startup screen must show the welcome text with the build version, got: {texts:?}"
    );
}

#[test]
fn startup_message_text_is_rendered() {
    let window = new_window();
    window.set_startup_visible(true);
    window.set_startup_message(ninfer_monitor::startup::MSG_NO_FOLDER.into());
    let _ = window.window().take_snapshot();
    let texts: Vec<String> = i_slint_backend_testing::ElementQuery::from_root(&window)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter_map(|e| e.accessible_label().map(|s| s.to_string()))
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t == ninfer_monitor::startup::MSG_NO_FOLDER),
        "startup screen must show the message text, got: {texts:?}"
    );
}

#[test]
fn startup_folder_input_round_trips() {
    let window = new_window();
    window.set_startup_visible(true);
    assert_eq!(window.get_startup_folder(), "");
    window.set_startup_folder("C:\\ninfer".into());
    assert_eq!(window.get_startup_folder(), "C:\\ninfer");
}

#[test]
fn enter_key_activates_focused_button() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    send_key(&window, '\t');
    send_key(&window, '\n');
    assert_eq!(last_action(&action), "tab-changed:0");
}

#[test]
fn space_key_activates_focused_button() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    send_key(&window, '\t');
    send_key(&window, ' ');
    assert_eq!(last_action(&action), "tab-changed:0");
}

#[test]
fn unrelated_key_does_not_activate_button() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    send_key(&window, '\t');
    send_key(&window, 'a');
    assert_eq!(last_action(&action), "");
}

#[test]
fn server_info_panel_takes_remaining_inner_area() {
    let window = new_window();
    let size = |id: &str| {
        ElementHandle::find_by_element_id(&window, id)
            .next()
            .unwrap_or_else(|| panic!("element not found: {id}"))
            .size()
    };
    // The inner area is tiled by the request table, the second splitter, and
    // the server-info panel (which takes whatever is left).
    let inner = size("MainWindow::inner-area");
    let table = size("MainWindow::request-table");
    let splitter = size("MainWindow::splitter2");
    let info = size("MainWindow::server-info");
    assert!(
        (inner.height - table.height - splitter.height - info.height).abs() < 1.0,
        "table + splitter + server-info must tile the inner area: \
         inner={:.1} table={:.1} splitter={:.1} info={:.1}",
        inner.height,
        table.height,
        splitter.height,
        info.height
    );
}

#[test]
fn server_info_properties_default_to_no_data() {
    let window = new_window();
    assert_eq!(window.get_server_info_summary(), "No server info yet");
    assert_eq!(window.get_server_info_model(), "—");
    assert_eq!(window.get_server_info_engine(), "—");
    assert_eq!(window.get_server_info_server(), "—");
}

#[test]
fn server_info_properties_round_trip() {
    let window = new_window();
    window.set_server_info_summary("RTX 5090 · qwen3.8-27b-nvfp4".into());
    window.set_server_info_model("qwen3_8_27b (nvfp4)".into());
    window.set_server_info_engine("capacity 240 000".into());
    window.set_server_info_server("0.0.0.0:8080".into());
    assert_eq!(
        window.get_server_info_summary(),
        "RTX 5090 · qwen3.8-27b-nvfp4"
    );
    assert_eq!(window.get_server_info_model(), "qwen3_8_27b (nvfp4)");
    assert_eq!(window.get_server_info_engine(), "capacity 240 000");
    assert_eq!(window.get_server_info_server(), "0.0.0.0:8080");
}

#[test]
fn placeholder_defaults_match_wireframe() {
    let window = new_window();
    assert_eq!(window.get_install_folder(), "");
    assert_eq!(window.get_connection_status(), "—");
    assert_eq!(window.get_last_event(), "—");
    assert_eq!(window.get_errors_text(), "Errors: 0");
    assert_eq!(window.get_skipped_text(), "Skipped 0");
}

#[test]
fn kpi_cards_default_to_no_data() {
    let window = new_window();
    assert_eq!(window.get_kpi_decode_value(), "—");
    assert_eq!(window.get_kpi_prefill_value(), "—");
    assert_eq!(window.get_kpi_active_value(), "—");
    assert_eq!(window.get_kpi_waiting_value(), "—");
    assert_eq!(window.get_kpi_ttft_value(), "—");
    assert_eq!(window.get_kpi_decode_lat_value(), "—");
    assert_eq!(window.get_kpi_cache_value(), "—");
    assert_eq!(window.get_kpi_spec_value(), "—");
    assert_eq!(window.get_kpi_ttft_unit(), "");
    assert_eq!(window.get_kpi_decode_lat_unit(), "");
    assert_eq!(window.get_kpi_cache_unit(), "");
    assert_eq!(window.get_kpi_spec_unit(), "");
    assert_eq!(window.get_kpi_decode_trend(), 0);
    assert_eq!(window.get_kpi_prefill_trend(), 0);
    assert_eq!(window.get_kpi_active_trend(), 0);
    assert_eq!(window.get_kpi_waiting_trend(), 0);
    assert_eq!(window.get_kpi_ttft_trend(), 0);
    assert_eq!(window.get_kpi_decode_lat_trend(), 0);
    assert_eq!(window.get_kpi_cache_trend(), 0);
    assert_eq!(window.get_kpi_spec_trend(), 0);
}

#[test]
fn kpi_properties_round_trip() {
    let window = new_window();
    window.set_kpi_decode_value("123.9".into());
    window.set_kpi_decode_trend(1);
    window.set_kpi_active_value("1".into());
    window.set_kpi_waiting_value("2".into());
    assert_eq!(window.get_kpi_decode_value(), "123.9");
    assert_eq!(window.get_kpi_decode_trend(), 1);
    assert_eq!(window.get_kpi_active_value(), "1");
    assert_eq!(window.get_kpi_waiting_value(), "2");
}

#[test]
fn request_table_renders_one_row_per_item() {
    let window = new_window();
    set_rows(
        &window,
        vec![
            make_row("1", "done"),
            make_row("2", "done"),
            make_row("3", "in-flight"),
        ],
    );
    let count = ElementHandle::find_by_element_id(&window, "RequestTable::row-el").count();
    assert_eq!(count, 3, "expected 3 table rows");
}

#[test]
fn request_table_empty_by_default() {
    let window = new_window();
    let count = ElementHandle::find_by_element_id(&window, "RequestTable::row-el").count();
    assert_eq!(count, 0, "expected no table rows by default");
}

#[test]
fn row_click_expands_detail_inline() {
    let window = new_window();
    set_expandable_rows(&window, vec![make_row("42", "done"), make_row("7", "done")]);
    assert_eq!(
        first_row(&window).size().height,
        32.0,
        "row starts collapsed"
    );
    assert_eq!(
        first_row_ta(&window)
            .accessible_label()
            .unwrap()
            .to_string(),
        "Expand request 42",
        "collapsed row shows the expand icon"
    );
    click(&window, "TableRow::ta");
    assert!(
        first_row(&window).size().height > 32.0,
        "the clicked row must expand inline with its detail"
    );
    assert_eq!(
        first_row_ta(&window)
            .accessible_label()
            .unwrap()
            .to_string(),
        "Collapse request 42",
        "the expanded row shows the collapse icon"
    );
}

#[test]
fn row_click_expands_when_table_is_scrollable() {
    let window = new_window();
    let rows: Vec<RequestRow> = (0..50).map(|i| make_row(&i.to_string(), "done")).collect();
    set_expandable_rows(&window, rows);
    assert_eq!(
        first_row(&window).size().height,
        32.0,
        "row starts collapsed"
    );
    click(&window, "TableRow::ta");
    assert!(
        first_row(&window).size().height > 32.0,
        "a quick click must expand the row even when the table is scrollable"
    );
    assert_eq!(
        first_row_ta(&window)
            .accessible_label()
            .unwrap()
            .to_string(),
        "Collapse request 0",
        "the expanded row shows the collapse icon"
    );
}

#[test]
fn click_expanded_row_collapses_it() {
    let window = new_window();
    set_expandable_rows(&window, vec![make_row("42", "done")]);
    click(&window, "TableRow::ta");
    assert!(first_row(&window).size().height > 32.0, "row should expand");
    click(&window, "TableRow::ta");
    assert_eq!(
        first_row(&window).size().height,
        32.0,
        "clicking the expanded row again must collapse it"
    );
}

#[test]
fn multiple_rows_can_be_expanded() {
    let window = new_window();
    // The default table (table-frac 0.5) is too short to keep the second row
    // laid out once the first row expands inline (an expanded row is 144px,
    // past the 118px Flickable); give the table most of the inner area so both
    // clicked rows stay visible while the third remains below the viewport.
    window.set_table_frac(0.9);
    set_expandable_rows(
        &window,
        vec![
            make_row("1", "done"),
            make_row("2", "done"),
            make_row("3", "done"),
        ],
    );
    click(&window, "TableRow::ta");
    let second_ta = ElementHandle::find_by_element_id(&window, "TableRow::ta")
        .nth(1)
        .expect("second row touch area");
    second_ta.mock_single_click(PointerEventButton::Left);
    let heights: Vec<f32> = ElementHandle::find_by_element_id(&window, "RequestTable::row-el")
        .map(|e| e.size().height)
        .collect();
    assert!(
        heights[0] > 32.0 && heights[1] > 32.0,
        "both clicked rows must stay expanded: {heights:?}"
    );
    // The third row sits below the Flickable's visible area, so Slint does not
    // lay it out; verify it stayed collapsed via the content height the handler
    // computes from all rows (2 expanded + 1 collapsed + 2 spacings = 328px).
    assert_eq!(
        window.get_table_content_height(),
        328.0,
        "content height must reflect the collapsed third row"
    );
}

#[test]
fn expansion_resets_when_rows_cleared() {
    let window = new_window();
    set_expandable_rows(&window, vec![make_row("42", "done")]);
    click(&window, "TableRow::ta");
    assert!(first_row(&window).size().height > 32.0, "row should expand");
    set_rows(&window, vec![]);
    set_rows(&window, vec![make_row("42", "done")]);
    assert_eq!(
        first_row(&window).size().height,
        32.0,
        "expansion must not survive a table clear"
    );
}

// TODO(#12): Re-enable after fixing headless propagation from set_table_scroll_y to the
// `changed scroll-y` callback; the scroll value updates, but `table-follow` stays `true`.
#[test]
#[ignore]
fn table_follow_tracks_scroll_position() {
    let window = new_window();
    let rows = (0..100).map(|i| make_row(&i.to_string(), "done")).collect();
    set_rows(&window, rows);
    let _ = window.window().take_snapshot();
    i_slint_core::properties::ChangeTracker::run_change_handlers();
    assert!(window.get_table_follow());
    let flick = ElementHandle::find_by_element_id(&window, "RequestTable::flickable")
        .next()
        .expect("table flickable");
    let pos = flick.absolute_position();
    let size = flick.size();
    assert!(
        size.height > 100.0,
        "flickable size was {size:?} at {pos:?}"
    );
    window.set_table_scroll_y(-100.0);
    i_slint_core::properties::ChangeTracker::run_change_handlers();
    assert!(window.get_table_scroll_y() < -50.0);
    assert!(!window.get_table_follow());
}

// ---------- M7: request-table column widths ----------

/// Push one column width to the UI (M7), mirroring `set_column_width` in
/// `src/main.rs`.
fn set_col_width(window: &MainWindow, col: Column, width: u32) {
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

/// Wire the `column-changed` handler the same way the app does (src/main.rs):
/// a divider drag moves the two adjacent columns (the total stays constant,
/// both stay at or above their minimums) and pushes the updated widths to the
/// UI.
fn wire_column_handlers(window: &MainWindow) -> Arc<Mutex<ColumnWidths>> {
    let cols: Arc<Mutex<ColumnWidths>> = Arc::new(Mutex::new(ColumnWidths::default()));
    window.on_column_changed({
        let weak = window.as_weak();
        let cols = cols.clone();
        move |slider: i32, new_left: f32| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let Some((left, right)) = ninfer_monitor::columns::slider_columns(slider) else {
                return;
            };
            let updated = {
                let mut c = cols.lock().unwrap();
                *c = c.move_divider(left, right, new_left);
                c.widths
            };
            set_col_width(&w, left, updated[left.index()]);
            set_col_width(&w, right, updated[right.index()]);
        }
    });
    cols
}

fn first_slider_ta(window: &MainWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(window, "ColumnSlider::ta")
        .next()
        .expect("first column slider")
}

#[test]
fn header_has_nine_column_sliders() {
    let window = new_window();
    let count = ElementHandle::find_by_element_id(&window, "ColumnSlider::ta").count();
    assert_eq!(count, 9, "one slider between each pair of the 10 columns");
}

#[test]
fn column_slider_positions_track_the_column_widths() {
    let window = new_window();
    wire_column_handlers(&window);
    let _ = window.window().take_snapshot();
    let s0 = first_slider_ta(&window);
    let s1 = ElementHandle::find_by_element_id(&window, "ColumnSlider::ta")
        .nth(1)
        .expect("second column slider");
    let x0 = s0.absolute_position().x;
    let x1 = s1.absolute_position().x;

    window.set_col_id(60.0);
    window.set_col_time(125.0);
    let _ = window.window().take_snapshot();

    assert_eq!(
        s0.absolute_position().x - x0,
        20.0,
        "the Id/Time slider follows the Id column (40 -> 60)"
    );
    assert_eq!(
        s1.absolute_position().x - x1,
        40.0,
        "the Time/Model slider follows the Id and Time columns (40 -> 60, 105 -> 125)"
    );
}

#[test]
fn column_slider_drag_moves_both_columns() {
    let window = new_window();
    let cols = wire_column_handlers(&window);

    // Drag the Id/Time divider 10px to the right.
    drag_first_slider(&window, 10.0);

    let id = window.get_col_id();
    let time = window.get_col_time();
    assert!(id > 40.0, "the left column grows: {id}");
    assert!(time < 105.0, "the right column shrinks: {time}");
    assert_eq!(
        id + time,
        145.0,
        "the total width of the two columns stays constant"
    );
    let c = cols.lock().unwrap();
    assert_eq!(c.width(Column::Id), id as u32, "the state follows the UI");
    assert_eq!(c.width(Column::Time), time as u32);
    assert_eq!(
        c.total(),
        ColumnWidths::default().total(),
        "the grand total is constant"
    );
}

/// Drag the first slider (Id/Time) by `dx` px from its center.
fn drag_first_slider(window: &MainWindow, dx: f32) {
    let _ = window.window().take_snapshot();
    let slider = first_slider_ta(window);
    let pos = slider.absolute_position();
    slider.mock_drag(
        i_slint_core::api::LogicalPosition::new(pos.x + dx, pos.y),
        PointerEventButton::Left,
    );
}

#[test]
fn column_slider_drag_clamps_at_the_right_minimum() {
    // Drag far past the right minimum: Time stays at its minimum (85px), Id
    // takes the rest of the pair (40 + 105 - 85 = 60).
    let window = new_window();
    wire_column_handlers(&window);
    drag_first_slider(&window, 500.0);
    assert_eq!(
        window.get_col_time(),
        85.0,
        "Time cannot shrink below its minimum"
    );
    assert_eq!(window.get_col_id(), 60.0, "Id takes the rest of the pair");
}

#[test]
fn column_slider_drag_clamps_at_the_left_minimum() {
    // Drag far past the left minimum: Id stays at its minimum (32px), Time
    // takes the rest of the pair (40 + 105 - 32 = 113).
    let window = new_window();
    wire_column_handlers(&window);
    drag_first_slider(&window, -80.0);
    assert_eq!(
        window.get_col_id(),
        32.0,
        "Id cannot shrink below its minimum"
    );
    assert_eq!(
        window.get_col_time(),
        113.0,
        "Time takes the rest of the pair"
    );
}

// ---------- M8: update check ----------

/// The accessible text of every visible `Text` in the update dialog.
fn dialog_texts(window: &MainWindow) -> Vec<String> {
    i_slint_backend_testing::ElementQuery::from_root(window)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter_map(|e| e.accessible_label().map(|s| s.to_string()))
        .collect()
}

#[test]
fn update_button_hidden_by_default() {
    let window = new_window();
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::btn-update").count();
    assert_eq!(count, 0, "update button hidden when no update is available");
    window.set_update_available(true);
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::btn-update").count();
    assert_eq!(
        count, 1,
        "update button visible when an update is available"
    );
}

#[test]
fn update_button_fires_callback() {
    let window = new_window();
    window.set_update_available(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_update_clicked({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "MainWindow::btn-update");
    assert!(*fired.lock().unwrap(), "update callback not fired");
}

#[test]
fn bottom_tab_bar_buttons_match_the_top_tab_buttons() {
    let window = new_window();
    let _ = window.window().take_snapshot();
    let size = |id: &str| {
        ElementHandle::find_by_element_id(&window, id)
            .next()
            .unwrap()
            .size()
    };
    let top = size("MainWindow::tab-control");
    let about = size("MainWindow::btn-about");
    assert!(
        (about.width - top.width).abs() < 1.0 && (about.height - top.height).abs() < 1.0,
        "About button {about:?} must match the tab button size {top:?}"
    );
    window.set_update_available(true);
    let _ = window.window().take_snapshot();
    let update = size("MainWindow::btn-update");
    assert!(
        (update.width - top.width).abs() < 1.0 && (update.height - top.height).abs() < 1.0,
        "Update button {update:?} must match the tab button size {top:?}"
    );
}

#[test]
fn update_dialog_hidden_by_default() {
    let window = new_window();
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::update-panel").count();
    assert_eq!(count, 0, "update dialog hidden by default");
    window.set_update_visible(true);
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::update-panel").count();
    assert_eq!(count, 1, "update dialog visible when update-visible");
}

#[test]
fn update_ok_fires_install_callback() {
    let window = new_window();
    window.set_update_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_update_install({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "UpdatePanel::btn-ok");
    assert!(*fired.lock().unwrap(), "OK must fire the install callback");
}

#[test]
fn update_ok_button_is_hidden_without_show_ok() {
    let window = new_window();
    window.set_update_visible(true);
    let count = ElementHandle::find_by_element_id(&window, "UpdatePanel::btn-ok").count();
    assert_eq!(count, 1, "OK is visible by default");
    window.set_update_ok_visible(false);
    let count = ElementHandle::find_by_element_id(&window, "UpdatePanel::btn-ok").count();
    assert_eq!(
        count, 0,
        "OK must be hidden when update-ok-visible is false"
    );
}

#[test]
fn update_download_status_and_error_are_rendered() {
    let window = new_window();
    window.set_update_version("0.3.0".into());
    window.set_update_visible(true);
    window.set_update_downloading(true);
    let _ = window.window().take_snapshot();
    let texts = dialog_texts(&window);
    assert!(
        texts.iter().any(|t| t == "Downloading version 0.3.0..."),
        "downloading state must show the download status, got: {texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|t| t.starts_with("Do you want to install")),
        "the version question must be hidden while downloading, got: {texts:?}"
    );
    window.set_update_downloading(false);
    window.set_update_download_error("the installer download failed: network down".into());
    let _ = window.window().take_snapshot();
    let texts = dialog_texts(&window);
    assert!(
        texts
            .iter()
            .any(|t| t == "the installer download failed: network down"),
        "the download error must be shown, got: {texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|t| t.starts_with("Do you want to install")),
        "the version question must be hidden on error, got: {texts:?}"
    );
}

#[test]
fn update_cancel_closes_dialog() {
    let window = new_window();
    window.set_update_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_update_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "UpdatePanel::btn-cancel");
    assert!(
        *fired.lock().unwrap(),
        "Cancel must close the update dialog"
    );
}

#[test]
fn update_escape_closes_dialog() {
    let window = new_window();
    window.set_update_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_update_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    // Click the notes row (away from the link) so the press lands on the
    // click-blocker and the scope gains focus.
    click(&window, "UpdatePanel::notes-row");
    send_key(&window, '\u{1b}');
    assert!(
        *fired.lock().unwrap(),
        "Escape must close the update dialog"
    );
}

#[test]
fn update_release_notes_link_fires_callback() {
    let window = new_window();
    window.set_update_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_release_notes({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "UpdatePanel::ta");
    assert!(
        *fired.lock().unwrap(),
        "the release notes link must fire the release-notes callback"
    );
}

#[test]
fn update_dialog_version_text_is_rendered() {
    let window = new_window();
    window.set_update_version("0.3.0".into());
    window.set_update_visible(true);
    let _ = window.window().take_snapshot();
    let texts: Vec<String> = i_slint_backend_testing::ElementQuery::from_root(&window)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter_map(|e| e.accessible_label().map(|s| s.to_string()))
        .collect();
    assert!(
        texts.iter().any(|t| t == "New version available"),
        "update dialog must show the title, got: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t == "Do you want to install new release version 0.3.0?"),
        "update dialog must show the version question, got: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "Release notes"),
        "update dialog must show the release notes link, got: {texts:?}"
    );
}

#[test]
fn update_dialog_hugs_content_vertically() {
    let window = new_window();
    window.set_update_visible(true);
    let panel = ElementHandle::find_by_element_id(&window, "MainWindow::update-panel")
        .next()
        .unwrap();
    let size = panel.size();
    assert_eq!(size.width, 420.0, "update panel keeps its 420px width");
    assert!(
        (150.0..400.0).contains(&size.height),
        "update panel height is content-based: {size:?}"
    );
}

#[test]
fn latest_version_dialog_hidden_by_default() {
    let window = new_window();
    let count =
        ElementHandle::find_by_element_id(&window, "MainWindow::latest-version-panel").count();
    assert_eq!(count, 0, "latest-version dialog hidden by default");
    window.set_latest_version_visible(true);
    let count =
        ElementHandle::find_by_element_id(&window, "MainWindow::latest-version-panel").count();
    assert_eq!(
        count, 1,
        "latest-version dialog visible when latest-version-visible"
    );
}

#[test]
fn latest_version_ok_closes_dialog() {
    let window = new_window();
    window.set_latest_version_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_latest_version_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "LatestVersionPanel::btn-ok");
    assert!(
        *fired.lock().unwrap(),
        "OK must close the latest-version dialog"
    );
}

#[test]
fn latest_version_escape_closes_dialog() {
    let window = new_window();
    window.set_latest_version_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_latest_version_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    // The click lands on the panel's click-blocker, focusing the dialog scope.
    click(&window, "LatestVersionPanel::latest-version-text");
    send_key(&window, '\u{1b}');
    assert!(
        *fired.lock().unwrap(),
        "Escape must close the latest-version dialog"
    );
}

#[test]
fn latest_version_backdrop_click_closes_dialog() {
    let window = new_window();
    window.set_latest_version_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_latest_version_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    // A point on the backdrop, outside the centered panel.
    click_at(&window, 50.0, 50.0);
    assert!(
        *fired.lock().unwrap(),
        "a backdrop click must close the latest-version dialog"
    );
}

#[test]
fn latest_version_message_text_is_rendered() {
    let window = new_window();
    window.set_latest_version("0.2.0".into());
    window.set_latest_version_visible(true);
    let _ = window.window().take_snapshot();
    let texts: Vec<String> = i_slint_backend_testing::ElementQuery::from_root(&window)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter_map(|e| e.accessible_label().map(|s| s.to_string()))
        .collect();
    assert!(
        texts.iter().any(|t| t == "Up to date"),
        "latest-version dialog must show the title, got: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| { t == "You are currently running the latest available version 0.2.0." }),
        "latest-version dialog must show the message with the version, got: {texts:?}"
    );
}

#[test]
fn latest_version_dialog_hugs_content_vertically() {
    let window = new_window();
    window.set_latest_version_visible(true);
    let panel = ElementHandle::find_by_element_id(&window, "MainWindow::latest-version-panel")
        .next()
        .unwrap();
    let size = panel.size();
    assert_eq!(
        size.width, 420.0,
        "latest-version panel keeps its 420px width"
    );
    assert!(
        (100.0..300.0).contains(&size.height),
        "latest-version panel height is content-based: {size:?}"
    );
}

#[test]
fn settings_update_check_toggle_works() {
    let window = new_window();
    window.set_active_tab(2);
    assert!(window.get_settings_update_check_enabled());
    click(&window, "SettingsPanel::update-check-ta");
    assert!(
        !window.get_settings_update_check_enabled(),
        "clicking the checkbox must disable the update check"
    );
    click(&window, "SettingsPanel::update-check-ta");
    assert!(
        window.get_settings_update_check_enabled(),
        "clicking the checkbox again must re-enable the update check"
    );
}

#[test]
fn settings_update_days_slider_updates_value() {
    let window = new_window();
    window.set_active_tab(2);
    let initial = window.get_settings_update_check_days();
    click(&window, "SettingsPanel::update-days-slider");
    let updated = window.get_settings_update_check_days();
    assert_ne!(
        updated, initial,
        "clicking the slider must change the check interval"
    );
    assert!(
        (1..=14).contains(&updated),
        "check interval stays in range: {updated}"
    );
}

#[test]
fn settings_update_days_slider_inert_when_check_disabled() {
    let window = new_window();
    window.set_active_tab(2);
    window.set_settings_update_check_enabled(false);
    let initial = window.get_settings_update_check_days();
    click(&window, "SettingsPanel::update-days-slider");
    assert_eq!(
        window.get_settings_update_check_days(),
        initial,
        "the slider is inert while the update check is disabled"
    );
}

#[test]
fn settings_update_interval_label_formats_value() {
    let window = new_window();
    window.set_active_tab(2);
    let label = || {
        ElementHandle::find_by_element_id(&window, "SettingsPanel::update-interval-label")
            .next()
            .and_then(|e| e.accessible_label())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    window.set_settings_update_check_days(1);
    assert_eq!(label(), "Check interval: 1 day");
    window.set_settings_update_check_days(3);
    assert_eq!(label(), "Check interval: 3 days");
    window.set_settings_update_check_days(14);
    assert_eq!(label(), "Check interval: 14 days");
}

// ---------- v0.3 M3: Control tab ----------

fn make_config_row(name: &str, running: bool) -> ConfigRow {
    ConfigRow {
        name: name.into(),
        running,
        error: false,
    }
}

fn make_config_row_with_error(name: &str, running: bool, error: bool) -> ConfigRow {
    ConfigRow {
        name: name.into(),
        running,
        error,
    }
}

fn set_config_rows(window: &MainWindow, rows: Vec<ConfigRow>) {
    window.set_control_rows(slint::ModelRc::from(Rc::new(slint::VecModel::from(rows))));
}

#[test]
fn control_tab_shown_by_default() {
    let window = new_window();
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::control-tab").count();
    assert_eq!(
        count, 1,
        "the Control tab is shown by default (it is the active tab)"
    );
}

#[test]
fn control_tab_shows_when_active() {
    let window = new_window();
    window.set_active_tab(0);
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::control-tab").count();
    assert_eq!(count, 1, "the Control tab is shown when active");
}

#[test]
fn control_rows_render_in_the_list() {
    let window = new_window();
    window.set_active_tab(0);
    set_config_rows(
        &window,
        vec![
            make_config_row("my-server", false),
            make_config_row("bench-run", false),
        ],
    );
    let _ = window.window().take_snapshot();
    let count = ElementHandle::find_by_element_id(&window, "ControlPanel::config-row").count();
    assert_eq!(count, 2, "one row per configuration");
}

#[test]
fn configs_list_shows_a_vertical_scrollbar_when_rows_overflow() {
    let window = new_window();
    window.set_active_tab(0);
    set_config_rows(
        &window,
        (0..30)
            .map(|index| make_config_row(&format!("Config {index}"), false))
            .collect(),
    );
    let _ = window.window().take_snapshot();
    let scroll = ElementHandle::find_by_element_id(&window, "ControlPanel::configs-scroll")
        .next()
        .unwrap();
    let bars = scroll
        .query_descendants()
        .match_type_name("ScrollBar")
        .find_all();
    assert!(
        !bars.is_empty(),
        "the configurations list shows a vertical scrollbar"
    );
}

#[test]
fn command_line_edit_shows_a_vertical_scrollbar_when_text_overflows() {
    let window = new_window();
    window.set_active_tab(0);
    set_config_rows(&window, vec![make_config_row("my-server", false)]);
    window.set_control_selected(0);
    window.set_control_command_line(
        (0..30)
            .map(|index| format!("--option-{index} value-{index}"))
            .collect::<Vec<_>>()
            .join("\n")
            .into(),
    );
    let _ = window.window().take_snapshot();
    let edit = ElementHandle::find_by_element_id(&window, "ControlPanel::command-line-input")
        .next()
        .unwrap();
    let bars = edit
        .query_descendants()
        .match_type_name("ScrollBar")
        .find_all();
    assert!(
        !bars.is_empty(),
        "the command-line editor shows a vertical scrollbar"
    );
}

#[test]
fn control_error_row_and_status_are_rendered() {
    let window = new_window();
    window.set_active_tab(0);
    set_config_rows(
        &window,
        vec![
            make_config_row_with_error("failed", false, true),
            make_config_row("healthy", false),
        ],
    );
    window.set_control_selected(0);
    window.set_control_status("Error: the process exited with exit code 3\nboom".into());
    window.set_control_status_error(true);
    let _ = window.window().take_snapshot();
    let count = ElementHandle::find_by_element_id(&window, "ControlPanel::config-row").count();
    assert_eq!(count, 2, "one row per configuration");
    assert!(
        window.get_control_status_error(),
        "the Control-tab status line is flagged as an error"
    );
}

#[test]
fn control_add_button_fires_callback() {
    let window = new_window();
    window.set_active_tab(0);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_add({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "ControlPanel::btn-add");
    assert!(
        *fired.lock().unwrap(),
        "Add must fire the control-add callback"
    );
}

#[test]
fn control_delete_button_opens_confirmation_when_enabled() {
    let window = new_window();
    window.set_active_tab(0);
    set_config_rows(
        &window,
        vec![
            make_config_row("my-server", false),
            make_config_row("bench-run", false),
        ],
    );
    window.set_control_selected(1);
    window.set_control_delete_enabled(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_delete({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "ControlPanel::btn-delete");
    assert!(
        window.get_control_delete_confirm_visible(),
        "Delete must open the confirmation dialog when enabled"
    );
    assert_eq!(
        window.get_control_delete_confirm_name(),
        "bench-run",
        "the confirmation dialog must name the selected configuration"
    );
    assert!(
        !*fired.lock().unwrap(),
        "Delete must not delete before the confirmation is accepted"
    );
}

#[test]
fn control_delete_button_inert_when_disabled() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_delete_enabled(false);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_delete({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "ControlPanel::btn-delete");
    assert!(
        !window.get_control_delete_confirm_visible(),
        "Delete must not open the confirmation dialog while disabled"
    );
    assert!(
        !*fired.lock().unwrap(),
        "Delete must not fire while disabled"
    );
}

#[test]
fn control_delete_confirm_hidden_by_default() {
    let window = new_window();
    let count =
        ElementHandle::find_by_element_id(&window, "MainWindow::control-delete-confirm-panel")
            .count();
    assert_eq!(
        count, 0,
        "the delete confirmation dialog is hidden by default"
    );
    window.set_control_delete_confirm_visible(true);
    let count =
        ElementHandle::find_by_element_id(&window, "MainWindow::control-delete-confirm-panel")
            .count();
    assert_eq!(
        count, 1,
        "the delete confirmation dialog is shown when visible"
    );
}

#[test]
fn control_delete_confirm_accept_deletes_and_hides() {
    let window = new_window();
    window.set_active_tab(0);
    set_config_rows(&window, vec![make_config_row("my-server", false)]);
    window.set_control_selected(0);
    window.set_control_delete_confirm_visible(true);
    window.set_control_delete_confirm_name("my-server".into());
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_delete({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "ConfirmDeletePanel::btn-delete");
    assert!(
        *fired.lock().unwrap(),
        "accepting the confirmation must fire the control-delete callback"
    );
    assert!(
        !window.get_control_delete_confirm_visible(),
        "accepting the confirmation must hide the dialog"
    );
}

#[test]
fn control_delete_confirm_cancel_hides() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_delete_confirm_visible(true);
    window.set_control_delete_confirm_name("my-server".into());
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_delete({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "ConfirmDeletePanel::btn-cancel");
    assert!(
        !window.get_control_delete_confirm_visible(),
        "Cancel must hide the confirmation dialog"
    );
    assert!(
        !*fired.lock().unwrap(),
        "Cancel must not delete the configuration"
    );
}

#[test]
fn control_delete_confirm_escape_closes() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_delete_confirm_visible(true);
    window.set_control_delete_confirm_name("my-server".into());
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_delete({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "ConfirmDeletePanel::question-text");
    send_key(&window, '\u{1b}');
    assert!(
        !window.get_control_delete_confirm_visible(),
        "Escape must close the confirmation dialog"
    );
    assert!(
        !*fired.lock().unwrap(),
        "Escape must not delete the configuration"
    );
}

#[test]
fn control_delete_confirm_backdrop_click_closes() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_delete_confirm_visible(true);
    window.set_control_delete_confirm_name("my-server".into());
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_delete({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click_at(&window, 50.0, 50.0);
    assert!(
        !window.get_control_delete_confirm_visible(),
        "a backdrop click must close the confirmation dialog"
    );
    assert!(
        !*fired.lock().unwrap(),
        "a backdrop click must not delete the configuration"
    );
}

#[test]
fn control_delete_confirm_panel_hugs_content_vertically() {
    let window = new_window();
    window.set_control_delete_confirm_visible(true);
    window.set_control_delete_confirm_name("my-server".into());
    let panel =
        ElementHandle::find_by_element_id(&window, "MainWindow::control-delete-confirm-panel")
            .next()
            .unwrap();
    let size = panel.size();
    assert_eq!(
        size.width, 380.0,
        "the confirmation panel keeps its 380px width"
    );
    assert!(
        (120.0..260.0).contains(&size.height),
        "the confirmation panel height is content-based: {size:?}"
    );
}

#[test]
fn control_row_click_fires_selected_callback_with_index() {
    let window = new_window();
    window.set_active_tab(0);
    set_config_rows(
        &window,
        vec![
            make_config_row("a", false),
            make_config_row("b", false),
            make_config_row("c", false),
        ],
    );
    let _ = window.window().take_snapshot();
    let index: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    window.on_control_config_selected({
        let index = index.clone();
        move |i: i32| {
            *index.lock().unwrap() = Some(i);
        }
    });
    let second_ta = ElementHandle::find_by_element_id(&window, "ControlPanel::row-ta")
        .nth(1)
        .expect("second row touch area");
    second_ta.mock_single_click(PointerEventButton::Left);
    assert_eq!(
        *index.lock().unwrap(),
        Some(1),
        "the second row must report index 1"
    );
}

#[test]
fn control_start_button_fires_callback_when_enabled() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_start_enabled(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_start({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "ControlPanel::btn-start");
    assert!(
        *fired.lock().unwrap(),
        "Start must fire the control-start callback when enabled"
    );
}

#[test]
fn control_start_button_inert_when_disabled() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_start_enabled(false);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_start({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "ControlPanel::btn-start");
    assert!(
        !*fired.lock().unwrap(),
        "Start must not fire while disabled"
    );
}

#[test]
fn control_restart_button_fires_callback_when_enabled() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_restart_enabled(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_restart({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "ControlPanel::btn-restart");
    assert!(
        *fired.lock().unwrap(),
        "Restart must fire the control-restart callback when enabled"
    );
}

#[test]
fn control_stop_button_fires_callback_when_enabled() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_stop_enabled(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_control_stop({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "ControlPanel::btn-stop");
    assert!(
        *fired.lock().unwrap(),
        "Stop must fire the control-stop callback when enabled"
    );
}

#[test]
fn control_status_text_is_rendered() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_status("Added.".into());
    let _ = window.window().take_snapshot();
    let texts: Vec<String> = i_slint_backend_testing::ElementQuery::from_root(&window)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter_map(|e| e.accessible_label().map(|s| s.to_string()))
        .collect();
    assert!(
        texts.iter().any(|t| t == "Added."),
        "the status line must show the last operation's result, got: {texts:?}"
    );
}

#[test]
fn control_edit_panel_shows_placeholder_without_a_selection() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_selected(-1);
    let _ = window.window().take_snapshot();
    let texts: Vec<String> = i_slint_backend_testing::ElementQuery::from_root(&window)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter_map(|e| e.accessible_label().map(|s| s.to_string()))
        .collect();
    assert!(
        texts.iter().any(|t| t == "No configuration selected"),
        "the edit panel must show the placeholder when nothing is selected, got: {texts:?}"
    );
}

#[test]
fn control_name_and_command_line_round_trip() {
    let window = new_window();
    window.set_active_tab(0);
    assert_eq!(window.get_control_name(), "");
    assert_eq!(window.get_control_command_line(), "");
    window.set_control_name("my-server".into());
    window.set_control_command_line("--model x".into());
    assert_eq!(window.get_control_name(), "my-server");
    assert_eq!(window.get_control_command_line(), "--model x");
}

#[test]
fn control_split_tiles_the_middle_area() {
    let window = new_window();
    window.set_active_tab(0);
    window.set_control_split_frac(0.3);
    let _ = window.window().take_snapshot();
    let size = |id: &str| {
        ElementHandle::find_by_element_id(&window, id)
            .next()
            .unwrap_or_else(|| panic!("element not found: {id}"))
            .size()
    };
    let middle = size("ControlPanel::control-middle");
    let list = size("ControlPanel::configs-list");
    let splitter = size("ControlPanel::control-splitter");
    let edit = size("ControlPanel::edit-panel");
    assert!(
        (middle.height - list.height - splitter.height - edit.height).abs() < 1.0,
        "configs-list + splitter + edit-panel must tile the control middle: \
         middle={:.1} list={:.1} splitter={:.1} edit={:.1}",
        middle.height,
        list.height,
        splitter.height,
        edit.height
    );
}

#[test]
fn control_split_fraction_drives_list_height() {
    let window = new_window();
    window.set_active_tab(0);
    let list_height = |window: &MainWindow| {
        window.window().take_snapshot().ok();
        ElementHandle::find_by_element_id(window, "ControlPanel::configs-list")
            .next()
            .expect("configs-list element")
            .size()
            .height
    };
    window.set_control_split_frac(0.2);
    let small = list_height(&window);
    window.set_control_split_frac(0.6);
    let large = list_height(&window);
    assert!(
        large > small,
        "a larger control-split-frac must give the configurations list more height: \
         small={:.1} large={:.1}",
        small,
        large
    );
}
