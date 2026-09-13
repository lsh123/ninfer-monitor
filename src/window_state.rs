// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

//! Persistence and restoration of the main window's geometry (position, size,
//! and maximized state), including which monitor the window was on.
//!
//! The geometry is stored in logical pixels so it survives DPI changes. On
//! restore, the saved window is matched to a currently connected monitor by
//! its center point; if that monitor no longer exists, the caller falls back
//! to the default size/position. The minimized state is intentionally not
//! persisted.

use serde::Serialize;

/// The persisted main-window geometry, in logical pixels.
///
/// `x`/`y` is the top-left corner of the window (including its frame);
/// `width`/`height` is the client size (excluding the frame). `maximized`
/// records whether the window was maximized when the app closed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

/// A currently connected monitor, with its bounds in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// What to do with the window at startup.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RestorePlan {
    /// Restore the saved geometry (logical pixels) on the matched monitor.
    Restore {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        maximized: bool,
    },
    /// There is no saved state, or the saved monitor no longer exists: use
    /// the default size/position.
    Default,
}

/// The monitor whose bounds contain the point `(cx, cy)`, or `None`.
///
/// Bounds are half-open (`x <= cx < x + width`), so a point on a shared edge
/// belongs to the monitor to its right/bottom.
pub fn find_monitor(monitors: &[Monitor], cx: f32, cy: f32) -> Option<&Monitor> {
    monitors
        .iter()
        .find(|m| cx >= m.x && cx < m.x + m.width && cy >= m.y && cy < m.y + m.height)
}

/// Clamp the saved geometry so the window fits within `monitor`.
fn clamp_to_monitor(state: &WindowState, monitor: &Monitor) -> (f32, f32, f32, f32) {
    let width = state.width.min(monitor.width);
    let height = state.height.min(monitor.height);
    let x = state.x.clamp(monitor.x, monitor.x + monitor.width - width);
    let y = state
        .y
        .clamp(monitor.y, monitor.y + monitor.height - height);
    (x, y, width, height)
}

/// Decide the startup geometry from the saved state and the current monitors.
///
/// The saved window is matched to a monitor by its center point. If that
/// monitor is still connected, the geometry is restored (clamped to fit the
/// monitor). If it is gone, the default size/position is used. When no
/// monitors could be enumerated (non-Windows), the saved geometry is restored
/// as-is.
pub fn resolve_restore(saved: Option<&WindowState>, monitors: &[Monitor]) -> RestorePlan {
    let Some(saved) = saved else {
        return RestorePlan::Default;
    };
    if monitors.is_empty() {
        return RestorePlan::Restore {
            x: saved.x,
            y: saved.y,
            width: saved.width,
            height: saved.height,
            maximized: saved.maximized,
        };
    }
    let cx = saved.x + saved.width / 2.0;
    let cy = saved.y + saved.height / 2.0;
    match find_monitor(monitors, cx, cy) {
        Some(monitor) => {
            let (x, y, width, height) = clamp_to_monitor(saved, monitor);
            RestorePlan::Restore {
                x,
                y,
                width,
                height,
                maximized: saved.maximized,
            }
        }
        None => RestorePlan::Default,
    }
}

/// Parse the `window_state` object from a config JSON object, tolerantly.
///
/// Returns `None` when the object is absent or any of `x`/`y`/`width`/`height`
/// is missing, not a finite number, or non-positive, so a malformed entry can
/// never produce a bogus restore. `maximized` defaults to `false`.
pub fn parse_window_state(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Option<WindowState> {
    let get_f32 = |key: &str| {
        object.get(key).and_then(|v| v.as_f64()).and_then(|v| {
            let f = v as f32;
            f.is_finite().then_some(f)
        })
    };
    let (Some(x), Some(y), Some(width), Some(height)) = (
        get_f32("x"),
        get_f32("y"),
        get_f32("width"),
        get_f32("height"),
    ) else {
        return None;
    };
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let maximized = object
        .get("maximized")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Some(WindowState {
        x,
        y,
        width,
        height,
        maximized,
    })
}

/// The currently connected monitors, with bounds in logical pixels.
///
/// On Windows this enumerates the real monitors; on other platforms it returns
/// an empty list (the saved geometry is then restored as-is).
pub fn current_monitors() -> Vec<Monitor> {
    #[cfg(windows)]
    {
        win::enumerate()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// The crate's only `unsafe`, confined to Windows monitor enumeration
/// (multi-monitor geometry restore). Each `unsafe` block is soundness-justified
/// by an adjacent `// SAFETY:` comment: the `LPARAM` points at the `Vec` that
/// owns the enumeration, OS-supplied handles are treated as valid, and the
/// `GetDpiForMonitor` out-pointers reference writable `u32`s. On non-Windows
/// targets this module is not compiled and the crate is `unsafe`-free.
#[cfg(windows)]
mod win {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::core::BOOL;

    pub(super) fn enumerate() -> Vec<super::Monitor> {
        let mut monitors: Vec<super::Monitor> = Vec::new();
        let ptr = &mut monitors as *mut Vec<super::Monitor>;
        // SAFETY: `callback` only writes to the `Vec<Monitor>` pointed to by
        // `ptr` (passed through `LPARAM`) and reads monitor handles supplied
        // by the OS; the `Vec` outlives the `EnumDisplayMonitors` call.
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(callback), LPARAM(ptr as isize));
        }
        monitors
    }

    unsafe extern "system" fn callback(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _lprcmonitor: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        // SAFETY: `lparam` points to the `Vec<Monitor>` owned by `enumerate`,
        // which is alive for the whole enumeration; `hmonitor` is a valid
        // handle provided by the OS.
        unsafe {
            let monitors = &mut *(lparam.0 as *mut Vec<super::Monitor>);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(hmonitor, &mut info).as_bool() {
                let rect = info.rcMonitor;
                let scale = scale_factor(hmonitor);
                monitors.push(super::Monitor {
                    x: rect.left as f32 / scale,
                    y: rect.top as f32 / scale,
                    width: (rect.right - rect.left) as f32 / scale,
                    height: (rect.bottom - rect.top) as f32 / scale,
                });
            }
        }
        BOOL::from(true)
    }

    fn scale_factor(hmonitor: HMONITOR) -> f32 {
        let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
        // SAFETY: `hmonitor` is a valid handle from the enumeration callback
        // and the pointers reference writable `u32`s.
        let ok = unsafe { GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
            .is_ok_and(|_| dpi_x > 0);
        if ok { dpi_x as f32 / 96.0 } else { 1.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(x: f32, y: f32, w: f32, h: f32) -> Monitor {
        Monitor {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn state(x: f32, y: f32, w: f32, h: f32, maximized: bool) -> WindowState {
        WindowState {
            x,
            y,
            width: w,
            height: h,
            maximized,
        }
    }

    #[test]
    fn no_saved_state_yields_default() {
        let monitors = vec![monitor(0.0, 0.0, 1920.0, 1080.0)];
        assert_eq!(resolve_restore(None, &monitors), RestorePlan::Default);
    }

    #[test]
    fn saved_state_on_existing_monitor_is_restored() {
        let monitors = vec![
            monitor(0.0, 0.0, 1920.0, 1080.0),
            monitor(1920.0, 0.0, 2560.0, 1440.0),
        ];
        let saved = state(2000.0, 100.0, 800.0, 600.0, false);
        assert_eq!(
            resolve_restore(Some(&saved), &monitors),
            RestorePlan::Restore {
                x: 2000.0,
                y: 100.0,
                width: 800.0,
                height: 600.0,
                maximized: false,
            }
        );
    }

    #[test]
    fn saved_state_on_gone_monitor_yields_default() {
        let monitors = vec![monitor(0.0, 0.0, 1920.0, 1080.0)];
        let saved = state(2000.0, 100.0, 800.0, 600.0, false);
        assert_eq!(
            resolve_restore(Some(&saved), &monitors),
            RestorePlan::Default
        );
    }

    #[test]
    fn no_monitors_restores_saved_as_is() {
        let saved = state(2000.0, 100.0, 800.0, 600.0, true);
        assert_eq!(
            resolve_restore(Some(&saved), &[]),
            RestorePlan::Restore {
                x: 2000.0,
                y: 100.0,
                width: 800.0,
                height: 600.0,
                maximized: true,
            }
        );
    }

    #[test]
    fn geometry_is_clamped_to_monitor() {
        let monitors = vec![monitor(0.0, 0.0, 1920.0, 1080.0)];
        let saved = state(-100.0, -50.0, 2200.0, 1200.0, false);
        assert_eq!(
            resolve_restore(Some(&saved), &monitors),
            RestorePlan::Restore {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
                maximized: false,
            }
        );
    }

    #[test]
    fn maximized_state_is_preserved() {
        let monitors = vec![monitor(0.0, 0.0, 1920.0, 1080.0)];
        let saved = state(100.0, 100.0, 800.0, 600.0, true);
        assert!(matches!(
            resolve_restore(Some(&saved), &monitors),
            RestorePlan::Restore {
                maximized: true,
                ..
            }
        ));
    }

    #[test]
    fn find_monitor_by_center() {
        let monitors = vec![
            monitor(0.0, 0.0, 100.0, 100.0),
            monitor(100.0, 0.0, 100.0, 100.0),
        ];
        assert!(find_monitor(&monitors, 50.0, 50.0).is_some_and(|m| m.x == 0.0));
        assert!(find_monitor(&monitors, 150.0, 50.0).is_some_and(|m| m.x == 100.0));
        assert!(find_monitor(&monitors, 250.0, 50.0).is_none());
    }

    #[test]
    fn parse_window_state_round_trips() {
        let value: serde_json::Value = serde_json::json!({
            "x": 100.5,
            "y": 200.0,
            "width": 800.0,
            "height": 600.0,
            "maximized": true
        });
        let object = value.as_object().unwrap();
        assert_eq!(
            parse_window_state(object),
            Some(state(100.5, 200.0, 800.0, 600.0, true))
        );
    }

    #[test]
    fn parse_window_state_defaults_maximized() {
        let value: serde_json::Value =
            serde_json::json!({ "x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0 });
        let object = value.as_object().unwrap();
        assert_eq!(
            parse_window_state(object),
            Some(state(1.0, 2.0, 3.0, 4.0, false))
        );
    }

    #[test]
    fn parse_window_state_rejects_missing_or_bad_fields() {
        let cases = [
            serde_json::json!({}),
            serde_json::json!({ "x": 1.0, "y": 2.0, "width": 3.0 }),
            serde_json::json!({ "x": "a", "y": 2.0, "width": 3.0, "height": 4.0 }),
            serde_json::json!({ "x": 1.0, "y": 2.0, "width": 0.0, "height": 4.0 }),
            serde_json::json!({ "x": 1.0, "y": 2.0, "width": 3.0, "height": -4.0 }),
        ];
        for value in cases {
            let object = value.as_object().unwrap();
            assert!(parse_window_state(object).is_none(), "{value}");
        }
    }

    #[test]
    fn parse_window_state_rejects_non_finite_f32() {
        let value = serde_json::json!({
            "x": 1e308,
            "y": 2.0,
            "width": 3.0,
            "height": 4.0
        });
        let object = value.as_object().unwrap();
        assert!(parse_window_state(object).is_none());
    }
}
