// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

slint::include_modules!();

use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;

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

fn elem(window: &MainWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(window, id)
        .next()
        .unwrap_or_else(|| panic!("element not found: {id}"))
}

#[test]
fn status_change_does_not_shift_items() {
    let window = new_window();
    window.set_file_name("tests/fixtures/server.requests.jsonl".into());
    window.set_file_name_full("tests/fixtures/server.requests.jsonl".into());
    window.set_last_event("14:02:58.124".into());
    window.set_connection_status("Live".into());
    let _ = window.window().take_snapshot();

    let file_live = elem(&window, "StatusBar::file-text").absolute_position();
    let time_live = elem(&window, "StatusBar::time-label").absolute_position();

    window.set_connection_status("Paused".into());
    let _ = window.window().take_snapshot();
    let file_paused = elem(&window, "StatusBar::file-text").absolute_position();
    let time_paused = elem(&window, "StatusBar::time-label").absolute_position();
    assert_eq!(time_live, time_paused, "timestamp shifted on pause");
    assert_eq!(file_live, file_paused, "file name shifted on pause");

    window.set_connection_status("Disconnected".into());
    let _ = window.window().take_snapshot();
    let file_disc = elem(&window, "StatusBar::file-text").absolute_position();
    let time_disc = elem(&window, "StatusBar::time-label").absolute_position();
    assert_eq!(time_live, time_disc, "timestamp shifted on disconnect");
    assert_eq!(file_live, file_disc, "file name shifted on disconnect");

    let time_size = elem(&window, "StatusBar::time-label").size();
    let gap = file_live.x - (time_live.x + time_size.width);
    assert!((gap - 12.0).abs() < 0.5, "expected ~12px gap, got {gap}");
}

#[test]
fn file_measure_text_is_hidden() {
    let window = new_window();
    window.set_file_name("tests/fixtures/server.requests.jsonl".into());
    window.set_file_name_full("tests/fixtures/server.requests.jsonl".into());
    let snap1 = window.window().take_snapshot().expect("snapshot");
    let rgba1: Vec<u8> = snap1
        .as_slice()
        .iter()
        .flat_map(|p| [p.r, p.g, p.b, p.a])
        .collect();
    window.set_file_name_full("a_totally_different_long_name_for_measuring_purposes.jsonl".into());
    let snap2 = window.window().take_snapshot().expect("snapshot");
    let rgba2: Vec<u8> = snap2
        .as_slice()
        .iter()
        .flat_map(|p| [p.r, p.g, p.b, p.a])
        .collect();
    let differing = (0..rgba1.len().min(rgba2.len()) / 4)
        .filter(|&i| rgba1[i * 4..i * 4 + 4] != rgba2[i * 4..i * 4 + 4])
        .count();
    assert_eq!(differing, 0, "file measure text is leaking into the render");
}

#[test]
fn shortening_decision() {
    let window = new_window();
    let long = "d:/some/very/long/path/with/many/components/in/it/server.requests.jsonl";
    window.set_file_name_full(long.into());
    let _ = window.window().take_snapshot();

    let full_width = window.get_file_full_width();
    let available = window.get_file_available();
    let file_w = elem(&window, "StatusBar::file-text").size().width;
    assert!(
        (available - file_w).abs() < 1.0,
        "file-available must match the file text's actual layout width"
    );
    assert!(
        full_width <= available,
        "long name should fit at 1280px (full width shown)"
    );

    window
        .window()
        .set_size(slint::WindowSize::Logical(slint::LogicalSize::new(
            1024.0, HEIGHT,
        )));
    let _ = window.window().take_snapshot();
    let very_long = format!(
        "d:/{}server.requests.jsonl",
        "a_very_long_directory_name/".repeat(6)
    );
    window.set_file_name_full(very_long.into());
    let _ = window.window().take_snapshot();
    let full_width = window.get_file_full_width();
    let available = window.get_file_available();
    assert!(
        full_width > available,
        "very long name must not fit at 1024px (shortened form shown)"
    );
}
