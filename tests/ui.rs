// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

slint::include_modules!();

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use i_slint_backend_testing::ElementHandle;
use ninfer_monitor::requests;
use slint::ComponentHandle;
use slint::platform::PointerEventButton;

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
        .map(|r| requests::detail_block_height(!r.detail_error.as_str().is_empty()))
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
    "header-bar",
    "widget-throughput",
    "widget-latency",
    "widget-cache",
    "widget-scheduler",
    "request-table",
    "server-info",
    "status-bar",
    "btn-open",
    "btn-pause",
    "btn-clear",
    "btn-settings",
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
    window.on_open_file({
        let last_action = last_action.clone();
        move || {
            *last_action.lock().unwrap() = "open-file".into();
        }
    });
    window.on_pause_resume({
        let weak = window.as_weak();
        let last_action = last_action.clone();
        move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let paused = !w.get_paused();
            w.set_paused(paused);
            w.set_connection_status(if paused { "Paused" } else { "Live" }.into());
            *last_action.lock().unwrap() = "pause-resume".into();
        }
    });
    window.on_clear({
        let last_action = last_action.clone();
        move || {
            *last_action.lock().unwrap() = "clear".into();
        }
    });
    window.on_window_changed({
        let last_action = last_action.clone();
        move |ms| {
            *last_action.lock().unwrap() = format!("window-changed:{ms}");
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
fn open_file_button_fires_callback() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    click(&window, "MainWindow::btn-open");
    assert_eq!(last_action(&action), "open-file");
}

#[test]
fn pause_resume_button_toggles_state() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    click(&window, "MainWindow::btn-pause");
    assert!(window.get_paused());
    assert_eq!(window.get_connection_status(), "Paused");
    assert_eq!(last_action(&action), "pause-resume");
    click(&window, "MainWindow::btn-pause");
    assert!(!window.get_paused());
    assert_eq!(window.get_connection_status(), "Live");
}

#[test]
fn clear_button_fires_callback() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    click(&window, "MainWindow::btn-clear");
    assert_eq!(last_action(&action), "clear");
}

#[test]
fn window_selector_button_fires_callback() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    click(&window, "MainWindow::win-1h");
    assert_eq!(last_action(&action), "window-changed:3600000");
}

#[test]
fn window_selector_30m_button_fires_callback() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    click(&window, "MainWindow::win-30m");
    assert_eq!(last_action(&action), "window-changed:1800000");
}

#[test]
fn settings_button_fires_callback() {
    let window = new_window();
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "MainWindow::btn-settings");
    assert!(*fired.lock().unwrap(), "settings callback not fired");
}

#[test]
fn settings_panel_hidden_by_default() {
    let window = new_window();
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::settings-panel").count();
    assert_eq!(count, 0, "settings panel hidden by default");
    window.set_settings_visible(true);
    let count = ElementHandle::find_by_element_id(&window, "MainWindow::settings-panel").count();
    assert_eq!(count, 1, "settings panel visible when settings-visible");
}

#[test]
fn settings_panel_save_fires_callback_with_current_values() {
    let window = new_window();
    window.set_settings_visible(true);
    window.set_settings_poll_ms(1500);
    window.set_settings_max_requests(250);
    let last: Arc<Mutex<Option<(i32, i32)>>> = Arc::new(Mutex::new(None));
    window.on_settings_save({
        let last = last.clone();
        move |poll_ms: i32, max_requests: i32| {
            *last.lock().unwrap() = Some((poll_ms, max_requests));
        }
    });
    click(&window, "SettingsPanel::btn-ok");
    let (poll, max_requests) = last.lock().unwrap().expect("save not fired");
    assert_eq!(poll, 1500);
    assert_eq!(max_requests, 250);
}

#[test]
fn settings_slider_click_updates_poll_ms() {
    let window = new_window();
    window.set_settings_visible(true);
    let initial = window.get_settings_poll_ms();
    click(&window, "SettingsPanel::poll-slider");
    let updated = window.get_settings_poll_ms();
    assert_ne!(
        updated, initial,
        "clicking the slider must change the poll interval"
    );
    assert!(
        (100..=5_000).contains(&updated),
        "poll interval stays in range: {updated}"
    );
}

#[test]
fn settings_poll_label_formats_value() {
    let window = new_window();
    window.set_settings_visible(true);
    let label = || {
        ElementHandle::find_by_element_id(&window, "SettingsPanel::poll-label")
            .next()
            .and_then(|e| e.accessible_label())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    window.set_settings_poll_ms(500);
    assert_eq!(label(), "Poll interval: 500 ms");
    window.set_settings_poll_ms(1500);
    assert_eq!(label(), "Poll interval: 1.5 s");
    window.set_settings_poll_ms(5_000);
    assert_eq!(label(), "Poll interval: 5 s");
}

#[test]
fn settings_slider_click_updates_max_requests() {
    let window = new_window();
    window.set_settings_visible(true);
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
    window.set_settings_visible(true);
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
fn settings_panel_cancel_fires_close() {
    let window = new_window();
    window.set_settings_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::btn-cancel");
    assert!(
        *fired.lock().unwrap(),
        "cancel must close the settings dialog"
    );
}

#[test]
fn settings_panel_close_x_fires_close() {
    let window = new_window();
    window.set_settings_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::btn-close-x");
    assert!(
        *fired.lock().unwrap(),
        "close-x must close the settings dialog"
    );
}

#[test]
fn settings_escape_closes_dialog() {
    let window = new_window();
    window.set_settings_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_settings_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::spacer");
    send_key(&window, '\u{1b}');
    assert!(
        *fired.lock().unwrap(),
        "Escape must close the settings dialog"
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
fn about_link_opens_about_dialog() {
    let window = new_window();
    window.set_settings_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_about({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "SettingsPanel::about-link");
    assert!(
        *fired.lock().unwrap(),
        "about link must fire the about callback"
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
fn about_close_x_closes_dialog() {
    let window = new_window();
    window.set_about_visible(true);
    let fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_about_close({
        let fired = fired.clone();
        move || {
            *fired.lock().unwrap() = true;
        }
    });
    click(&window, "AboutPanel::btn-close-x");
    assert!(
        *fired.lock().unwrap(),
        "close-x must close the about dialog"
    );
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
    click(&window, "AboutPanel::top-spacer");
    send_key(&window, '\u{1b}');
    assert!(*fired.lock().unwrap(), "Escape must close the about dialog");
}

#[test]
fn settings_escape_closes_about_not_settings_when_about_open() {
    let window = new_window();
    window.set_settings_visible(true);
    click(&window, "SettingsPanel::spacer");
    window.set_about_visible(true);
    let about_fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let settings_fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    window.on_about_close({
        let about_fired = about_fired.clone();
        move || {
            *about_fired.lock().unwrap() = true;
        }
    });
    window.on_settings_close({
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
        "Escape must not close settings while about is open"
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
fn enter_key_activates_focused_button() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    send_key(&window, '\t');
    send_key(&window, '\n');
    assert_eq!(last_action(&action), "open-file");
}

#[test]
fn space_key_activates_focused_button() {
    let window = new_window();
    let action = wire_stub_handlers(&window);
    send_key(&window, '\t');
    send_key(&window, ' ');
    assert_eq!(last_action(&action), "open-file");
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
    assert_eq!(window.get_server_info_gpu(), "—");
    assert_eq!(window.get_server_info_memory(), "—");
    assert_eq!(window.get_server_info_server(), "—");
}

#[test]
fn server_info_properties_round_trip() {
    let window = new_window();
    window.set_server_info_summary("RTX 5090 · qwen3.8-27b-nvfp4".into());
    window.set_server_info_model("qwen3_8_27b (nvfp4)".into());
    window.set_server_info_engine("capacity 240 000".into());
    window.set_server_info_gpu("NVIDIA GeForce RTX 5090".into());
    window.set_server_info_memory("weights 21.2/21.2 GiB".into());
    window.set_server_info_server("0.0.0.0:8080".into());
    assert_eq!(
        window.get_server_info_summary(),
        "RTX 5090 · qwen3.8-27b-nvfp4"
    );
    assert_eq!(window.get_server_info_model(), "qwen3_8_27b (nvfp4)");
    assert_eq!(window.get_server_info_engine(), "capacity 240 000");
    assert_eq!(window.get_server_info_gpu(), "NVIDIA GeForce RTX 5090");
    assert_eq!(window.get_server_info_memory(), "weights 21.2/21.2 GiB");
    assert_eq!(window.get_server_info_server(), "0.0.0.0:8080");
}

#[test]
fn placeholder_defaults_match_wireframe() {
    let window = new_window();
    assert_eq!(window.get_file_name(), "");
    assert_eq!(window.get_connection_status(), "—");
    assert!(!window.get_paused());
    assert!(window.get_controls_enabled());
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
